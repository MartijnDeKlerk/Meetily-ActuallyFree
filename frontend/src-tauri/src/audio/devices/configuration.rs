use anyhow::{anyhow, Result};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::AtomicU64;

lazy_static! {
    pub static ref LAST_AUDIO_CAPTURE: AtomicU64 = AtomicU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioTranscriptionEngine {
    Deepgram,
    WhisperTiny,
    WhisperDistilLargeV3,
    WhisperLargeV3Turbo,
    WhisperLargeV3,
}

impl fmt::Display for AudioTranscriptionEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioTranscriptionEngine::Deepgram => write!(f, "Deepgram"),
            AudioTranscriptionEngine::WhisperTiny => write!(f, "WhisperTiny"),
            AudioTranscriptionEngine::WhisperDistilLargeV3 => write!(f, "WhisperLarge"),
            AudioTranscriptionEngine::WhisperLargeV3Turbo => write!(f, "WhisperLargeV3Turbo"),
            AudioTranscriptionEngine::WhisperLargeV3 => write!(f, "WhisperLargeV3"),
        }
    }
}

impl Default for AudioTranscriptionEngine {
    fn default() -> Self {
        AudioTranscriptionEngine::WhisperLargeV3Turbo
    }
}

#[derive(Clone, Debug)]
pub struct DeviceControl {
    pub is_running: bool,
    pub is_paused: bool,
}

#[derive(Clone, Eq, PartialEq, Hash, Serialize, Debug, Deserialize)]
pub enum DeviceType {
    Input,
    Output,
}

#[derive(Clone, Eq, PartialEq, Hash, Serialize, Debug)]
pub struct AudioDevice {
    pub name: String,
    pub device_type: DeviceType,
}

impl AudioDevice {
    pub fn new(name: String, device_type: DeviceType) -> Self {
        AudioDevice { name, device_type }
    }

    pub fn from_name(name: &str, device_type: DeviceType) -> Result<Self> {
        if name.trim().is_empty() {
            return Err(anyhow!("Device name cannot be empty"));
        }

        // The frontend's toDeviceOptionValue() always appends "(input)"/
        // "(output)" when saving a preference (frontend/src/lib/audio-devices.ts).
        // Strip it here so the resolved name matches what devices are actually
        // listed/looked-up as (a PulseAudio description, a raw cpal name, a
        // "... (System Audio)" tag, ...) - but the caller's `device_type` is
        // always authoritative; a name with no suffix (or an unrecognized one)
        // is not an error.
        let lower = name.to_lowercase();
        let stripped = if lower.ends_with("(input)") {
            name.trim_end_matches("(input)").trim()
        } else if lower.ends_with("(output)") {
            name.trim_end_matches("(output)").trim()
        } else {
            name
        };

        Ok(AudioDevice::new(stripped.to_string(), device_type))
    }
}

impl fmt::Display for AudioDevice {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} ({})",
            self.name,
            match self.device_type {
                DeviceType::Input => "input",
                DeviceType::Output => "output",
            }
        )
    }
}

/// Parse audio device from a stored preference/name string.
///
/// The frontend always appends "(input)"/"(output)" when saving a preference
/// (see toDeviceOptionValue in frontend/src/lib/audio-devices.ts), so that
/// suffix is stripped here rather than used to infer the type - the caller
/// supplies the device type directly from context (which field the preference
/// came from), since a name with no recognizable suffix must still resolve.
pub fn parse_audio_device(name: &str, device_type: DeviceType) -> Result<AudioDevice> {
    AudioDevice::from_name(name, device_type)
}

/// Get device and config for audio operations
pub async fn get_device_and_config(
    audio_device: &AudioDevice,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    #[cfg(target_os = "windows")]
    {
        return super::platform::get_windows_device(audio_device);
    }

    #[cfg(not(target_os = "windows"))]
    {
        use cpal::traits::{DeviceTrait, HostTrait};

        let host = cpal::default_host();

        match audio_device.device_type {
            DeviceType::Input => {
                // Linux microphones are listed as PulseAudio/PipeWire source
                // descriptions (see configure_linux_audio), not raw cpal/ALSA
                // names, so validate against the Pulse server first - the ALSA
                // loop below only ever matches the degraded-mode fallback names
                // ("default"/"pipewire"/"pulse").
                #[cfg(target_os = "linux")]
                if crate::audio::capture::find_source_by_description(&audio_device.name).is_ok() {
                    let _guard = super::platform::linux::ALSA_GLOBAL_LOCK
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let dummy_device = host
                        .default_input_device()
                        .ok_or_else(|| anyhow!("No default input device"))?;
                    let config = dummy_device
                        .default_input_config()
                        .map_err(|e| anyhow!("Failed to get config: {}", e))?;
                    return Ok((dummy_device, config));
                }

                #[cfg(target_os = "linux")]
                let _guard = super::platform::linux::ALSA_GLOBAL_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());

                for device in host.input_devices()? {
                    if let Ok(name) = device.name() {
                        if name == audio_device.name {
                            let default_config = device
                                .default_input_config()
                                .map_err(|e| anyhow!("Failed to get default input config: {}", e))?;
                            return Ok((device, default_config));
                        }
                    }
                }
            }
            DeviceType::Output => {
                #[cfg(target_os = "macos")]
                {
                    // Use default host for all macOS output devices
                    // Core Audio backend uses direct cidre API for system capture, not cpal
                    for device in host.output_devices()? {
                        if let Ok(name) = device.name() {
                            if name == audio_device.name {
                                let default_config = device
                                    .default_output_config()
                                    .map_err(|e| anyhow!("Failed to get output config: {}", e))?;
                                return Ok((device, default_config));
                            }
                        }
                    }
                }

                #[cfg(target_os = "linux")]
                {
                    // For Linux system audio with "(System Audio)" suffix, we use native
                    // PulseAudio capture which doesn't go through CPAL. Validate by
                    // checking if the sink exists in PulseAudio.
                    if audio_device.name.contains("(System Audio)") {
                        use crate::audio::capture::pulse_linux;

                        // Strip "(System Audio)" to get the sink description
                        let sink_desc = audio_device.name
                            .strip_suffix(" (System Audio)")
                            .unwrap_or(&audio_device.name);

                        // Check if this sink exists in PulseAudio
                        match pulse_linux::find_monitor_source_by_description(sink_desc) {
                            Ok(_monitor_source) => {
                                // Device exists! Return a dummy CPAL device since the actual
                                // stream creation in stream.rs will use PulseCapture directly
                                let host = cpal::default_host();
                                let dummy_device = host
                                    .default_input_device()
                                    .ok_or_else(|| anyhow!("No default input device"))?;
                                let config = dummy_device
                                    .default_input_config()
                                    .map_err(|e| anyhow!("Failed to get config: {}", e))?;
                                return Ok((dummy_device, config));
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "PulseAudio sink '{}' not found: {}",
                                    sink_desc,
                                    e
                                ));
                            }
                        }
                    }

                    // For fallback ALSA devices, validate they exist in CPAL
                    // Serialize every alsa-lib call: see ALSA_GLOBAL_LOCK doc comment.
                    let _guard = super::platform::linux::ALSA_GLOBAL_LOCK
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());

                    if let Ok(pulse_host) = cpal::host_from_id(cpal::HostId::Alsa) {
                        for device in pulse_host.input_devices()? {
                            if let Ok(name) = device.name() {
                                if name == audio_device.name {
                                    let default_config = device
                                        .default_input_config()
                                        .map_err(|e| anyhow!("Failed to get default input config: {}", e))?;
                                    return Ok((device, default_config));
                                }
                            }
                        }
                    }
                }
            }
        }

        Err(anyhow!("Device not found: {}", audio_device.name))
    }
}