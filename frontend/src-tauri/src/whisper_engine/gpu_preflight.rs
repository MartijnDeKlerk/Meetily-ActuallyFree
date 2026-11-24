//! Out-of-process check that Whisper GPU inference can survive one encode.
//!
//! ggml's GPU backends call `abort()` on a failed BLAS/runtime call (for example
//! `CUBLAS_STATUS_INTERNAL_ERROR` from hipBLAS when the bundled ROCm userspace and
//! the system rocBLAS kernel files come from different ROCm releases). An abort
//! cannot be caught in-process, so the first GPU load of a model is tried in a
//! short-lived copy of this executable. Only a clean exit lets the app load the
//! model on the GPU; any other result loads the same model on the CPU.
//!
//! Ownership: the parent's cache owns the verdict for the lifetime of the app
//! process, keyed by model path and context parameters. A restart re-checks, which
//! is the expected recovery after the user repairs their GPU runtime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::acceleration::{WhisperCompiledBackend, WhisperContextAcceleration};

/// argv[1] that turns this executable into the preflight child (see `main.rs`).
pub const GPU_PREFLIGHT_ARG: &str = "--whisper-gpu-preflight";

/// Covers a cold-cache load of the largest catalog model plus one encode. A child
/// still running after this is treated as a failed check and killed.
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(120);

/// Child exit code when whisper reported an ordinary error instead of aborting.
const EXIT_WHISPER_ERROR: i32 = 3;
const EXIT_BAD_ARGS: i32 = 2;

/// Lines of child stderr kept for the log. ggml prints its error line, the failing
/// call, then sometimes a gdb backtrace, so the excerpt starts at the first error
/// line rather than at the end; the model-load banner before it is dropped.
const STDERR_EXCERPT_LINES: usize = 8;

/// Only the HIP build is checked for now. It is the backend with a reproduced
/// abort (ROCm 7.2.4 userspace bundled in an AppImage against system ROCm 10.1
/// kernel files). CUDA/Vulkan builds can abort the same way in principle, but
/// spawning the packaged executable has not been qualified on Windows.
pub fn requires_preflight(backend: WhisperCompiledBackend) -> bool {
    matches!(backend, WhisperCompiledBackend::HipBlas)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightVerdict {
    Usable,
    /// Human-readable reason, logged and shown to the user with the CPU fallback.
    Unusable(String),
}

type CacheKey = (PathBuf, i32, bool);

fn cache() -> &'static Mutex<HashMap<CacheKey, PreflightVerdict>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, PreflightVerdict>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs (or reuses) the out-of-process check for `model_path` with the GPU
/// parameters the caller is about to use in-process.
pub async fn check(model_path: &Path, acceleration: WhisperContextAcceleration) -> PreflightVerdict {
    let key = (
        model_path.to_path_buf(),
        acceleration.gpu_device,
        acceleration.flash_attn,
    );
    if let Some(verdict) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return verdict.clone();
    }

    let span = crate::diagnostics::Span::start(crate::diagnostics::Stage::WhisperGpuPreflight);
    let verdict = run_child(model_path, acceleration).await;
    match &verdict {
        PreflightVerdict::Usable => span.complete(),
        PreflightVerdict::Unusable(_) => span.result(&Err::<(), ()>(())),
    }

    // Concurrent first loads may both run the child; they reach the same verdict,
    // so last-writer-wins is harmless and keeps the lock out of the await.
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, verdict.clone());
    verdict
}

async fn run_child(model_path: &Path, acceleration: WhisperContextAcceleration) -> PreflightVerdict {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return PreflightVerdict::Unusable(format!(
                "could not locate the app executable for the GPU check: {error}"
            ))
        }
    };

    // The child inherits this process's environment, including the AppImage's
    // library path and ROCBLAS_TENSILE_LIBPATH, so it loads the same GPU runtime.
    let mut command = tokio::process::Command::new(exe);
    command
        .arg(GPU_PREFLIGHT_ARG)
        .arg(model_path)
        .arg(acceleration.gpu_device.to_string())
        .arg(if acceleration.flash_attn { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return PreflightVerdict::Unusable(format!("could not start the GPU check: {error}"))
        }
    };

    // Drain stderr concurrently so a chatty child can never block on a full pipe.
    let mut stderr = child.stderr.take();
    let stderr_reader = tokio::spawn(async move {
        let mut buffer = Vec::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_end(&mut buffer).await;
        }
        String::from_utf8_lossy(&buffer).into_owned()
    });

    let status = match tokio::time::timeout(PREFLIGHT_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            return PreflightVerdict::Unusable(format!("the GPU check could not be awaited: {error}"))
        }
        Err(_) => {
            let _ = child.kill().await;
            stderr_reader.abort();
            return PreflightVerdict::Unusable(format!(
                "the GPU check did not finish within {}s",
                PREFLIGHT_TIMEOUT.as_secs()
            ));
        }
    };

    let stderr = stderr_reader.await.unwrap_or_default();
    if status.success() {
        return PreflightVerdict::Usable;
    }

    let outcome = describe_exit(status);
    let detail = stderr_excerpt(&stderr);
    for line in detail.lines() {
        log::warn!("Whisper GPU check child: {line}");
    }
    PreflightVerdict::Unusable(match first_error_line(&detail) {
        Some(line) => format!("{outcome}: {line}"),
        None => outcome,
    })
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                6 => "SIGABRT",
                11 => "SIGSEGV",
                9 => "SIGKILL",
                _ => "a signal",
            };
            return format!("the GPU check crashed ({name})");
        }
    }
    match status.code() {
        Some(EXIT_WHISPER_ERROR) => "the GPU check failed to run Whisper".to_string(),
        Some(code) => format!("the GPU check exited with code {code}"),
        None => "the GPU check exited abnormally".to_string(),
    }
}

fn is_error_line(line: &str) -> bool {
    line.to_ascii_lowercase().contains("error")
}

/// From the first error line onwards, or the last lines when nothing names an error
/// (for example a bare SIGSEGV).
fn stderr_excerpt(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines
        .iter()
        .position(|line| is_error_line(line))
        .unwrap_or_else(|| lines.len().saturating_sub(STDERR_EXCERPT_LINES));
    let end = (start + STDERR_EXCERPT_LINES).min(lines.len());
    lines[start..end].join("\n")
}

/// Picks the line most likely to name the failure, e.g.
/// `ROCm error: CUBLAS_STATUS_INTERNAL_ERROR`.
fn first_error_line(tail: &str) -> Option<String> {
    tail.lines()
        .map(str::trim)
        .find(|line| is_error_line(line))
        .map(|line| line.chars().take(200).collect())
}

/// Tells the UI (app/layout.tsx) that `model_name` was loaded on the CPU. Called
/// by each load path that has an AppHandle, right after a load that fell back.
pub fn emit_fallback<R: tauri::Runtime>(app: &tauri::AppHandle<R>, model_name: &str, reason: &str) {
    use tauri::Emitter;
    if let Err(error) = app.emit(
        "whisper-gpu-fallback",
        serde_json::json!({ "modelName": model_name, "reason": reason }),
    ) {
        log::error!("Failed to emit whisper-gpu-fallback event: {error}");
    }
}

/// Entry point for the preflight child. Never returns.
///
/// Arguments after [`GPU_PREFLIGHT_ARG`]: model path, gpu device, flash-attn (0/1).
/// The child loads the model on the GPU and transcribes one second of silence;
/// Whisper pads input to a full 30 s window, so this runs the complete encoder and
/// one decode step - the same BLAS calls that abort during a real transcription.
pub fn run_child_and_exit() -> ! {
    let mut args = std::env::args().skip(2);
    let (Some(model_path), Some(gpu_device), Some(flash_attn)) = (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: {GPU_PREFLIGHT_ARG} <model> <gpu-device> <flash-attn 0|1>");
        std::process::exit(EXIT_BAD_ARGS);
    };
    let Ok(gpu_device) = gpu_device.parse::<i32>() else {
        eprintln!("invalid gpu device: {gpu_device}");
        std::process::exit(EXIT_BAD_ARGS);
    };

    if requires_preflight(WhisperCompiledBackend::current()) {
        // Idempotent; normally already set by the parent and inherited.
        super::WhisperEngine::configure_rocm_tensile_libpath();
    }

    match transcribe_silence(&model_path, gpu_device, flash_attn == "1") {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("Whisper GPU check error: {error}");
            std::process::exit(EXIT_WHISPER_ERROR);
        }
    }
}

fn transcribe_silence(model_path: &str, gpu_device: i32, flash_attn: bool) -> anyhow::Result<()> {
    let params = WhisperContextParameters {
        use_gpu: true,
        gpu_device,
        flash_attn,
        ..Default::default()
    };
    let context = WhisperContext::new_with_params(model_path, params)?;
    let mut state = context.create_state()?;

    let mut full = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    full.set_n_threads(1);
    full.set_language(Some("en"));
    full.set_no_context(true);
    full.set_single_segment(true);
    full.set_print_special(false);
    full.set_print_progress(false);
    full.set_print_realtime(false);
    full.set_print_timestamps(false);

    let one_second_of_silence = vec![0.0_f32; 16_000];
    state.full(full, &one_second_of_silence)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hip_builds_are_preflighted() {
        assert!(requires_preflight(WhisperCompiledBackend::HipBlas));
        for backend in [
            WhisperCompiledBackend::Cpu,
            WhisperCompiledBackend::Cuda,
            WhisperCompiledBackend::Vulkan,
            WhisperCompiledBackend::Metal,
        ] {
            assert!(!requires_preflight(backend));
        }
    }

    // Shape of the real child stderr from the ROCm 7.2.4-on-10.1 abort, including
    // the gdb backtrace ggml sometimes appends.
    const ROCM_ABORT_STDERR: &str = "\
whisper_model_load: model size    =  573,40 MB
whisper_init_state: compute buffer (decode) =  100,04 MB
ROCm error: CUBLAS_STATUS_INTERNAL_ERROR
  current device: 0, in function ggml_cuda_op_mul_mat_cublas at ggml-cuda.cu:1319
  hipblasGemmEx(ctx.cublas_handle(id), ...)
ggml-cuda.cu:96: ROCm error
#3  0x00005616fc513a10 in ggml_abort ()
#4  0x00005616fc513a72 in ggml_cuda_error(char const*, char const*, char const*, int, char const*) ()
#5  0x00005616fc514b00 in ggml_cuda_op_mul_mat_cublas ()
#6  0x00005616fc515c00 in ggml_cuda_mul_mat ()
#7  0x00005616fc516d00 in ggml_backend_graph_compute ()
#8  0x00005616fc517e00 in whisper_encode_internal ()
";

    #[test]
    fn reason_names_the_ggml_error_line_even_with_a_backtrace() {
        let excerpt = stderr_excerpt(ROCM_ABORT_STDERR);
        assert!(excerpt.starts_with("ROCm error: CUBLAS_STATUS_INTERNAL_ERROR"));
        assert_eq!(excerpt.lines().count(), STDERR_EXCERPT_LINES);
        assert!(!excerpt.contains("model size"));
        assert_eq!(
            first_error_line(&excerpt).as_deref(),
            Some("ROCm error: CUBLAS_STATUS_INTERNAL_ERROR")
        );
    }

    #[test]
    fn short_quiet_or_errorless_stderr_is_handled() {
        assert_eq!(stderr_excerpt(""), "");
        assert_eq!(first_error_line(""), None);
        assert_eq!(stderr_excerpt("one\n\n  \ntwo\n"), "one\ntwo");
        let many: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let excerpt = stderr_excerpt(&many);
        assert_eq!(excerpt.lines().next(), Some("line 12"));
        assert_eq!(first_error_line(&excerpt), None);
    }

    #[cfg(unix)]
    #[test]
    fn abort_and_error_exits_are_described() {
        use std::os::unix::process::ExitStatusExt;
        // Raw wait statuses: a signal number in the low bits, an exit code shifted by 8.
        let aborted = std::process::ExitStatus::from_raw(6);
        let whisper_error = std::process::ExitStatus::from_raw(EXIT_WHISPER_ERROR << 8);
        assert_eq!(describe_exit(aborted), "the GPU check crashed (SIGABRT)");
        assert_eq!(describe_exit(whisper_error), "the GPU check failed to run Whisper");
    }
}
