//! OS light/dark preference.
//!
//! WebKitGTK derives `prefers-color-scheme` from the GTK theme, which on KDE
//! (and other non-GTK desktops) does not follow the desktop's light/dark
//! setting. The XDG Settings portal is what browsers and GTK4 apps read
//! instead, so the frontend asks for it here. On other platforms the command
//! returns `None` and the frontend keeps using `prefers-color-scheme`.

use tauri::AppHandle;

pub const SYSTEM_COLOR_SCHEME_EVENT: &str = "system-color-scheme-changed";

/// "dark", "light", or "none" when the OS expresses no preference; `None` when
/// the OS cannot be asked at all.
#[tauri::command]
pub async fn get_system_color_scheme() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        return linux::read().await;
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Emit `system-color-scheme-changed` (payload "dark", "light" or "none") on OS changes.
pub fn watch(_app: &AppHandle) {
    #[cfg(target_os = "linux")]
    {
        let app = _app.clone();
        tauri::async_runtime::spawn(async move { linux::watch(app).await });
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::SYSTEM_COLOR_SCHEME_EVENT;
    use futures_util::StreamExt;
    use tauri::{AppHandle, Emitter};
    use zbus::zvariant::OwnedValue;

    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";

    /// Portal values: 0 = no preference, 1 = prefer dark, 2 = prefer light.
    fn label(value: u32) -> &'static str {
        match value {
            1 => "dark",
            2 => "light",
            _ => "none",
        }
    }

    async fn proxy(conn: &zbus::Connection) -> zbus::Result<zbus::Proxy<'static>> {
        zbus::Proxy::new(
            conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )
        .await
    }

    pub async fn read() -> Option<String> {
        let conn = zbus::Connection::session().await.ok()?;
        let proxy = proxy(&conn).await.ok()?;
        let value: OwnedValue = proxy.call("ReadOne", &(NAMESPACE, KEY)).await.ok()?;
        Some(label(u32::try_from(value).ok()?).to_string())
    }

    pub async fn watch(app: AppHandle) {
        let result: zbus::Result<()> = async {
            let conn = zbus::Connection::session().await?;
            let proxy = proxy(&conn).await?;
            let mut changes = proxy.receive_signal("SettingChanged").await?;
            while let Some(message) = changes.next().await {
                let Ok((namespace, key, value)) =
                    message.body().deserialize::<(String, String, OwnedValue)>()
                else {
                    continue;
                };
                if namespace != NAMESPACE || key != KEY {
                    continue;
                }
                // "none" is forwarded too: the frontend must drop a stale dark/light
                // override and fall back to prefers-color-scheme.
                if let Ok(value) = u32::try_from(value) {
                    let _ = app.emit(SYSTEM_COLOR_SCHEME_EVENT, label(value));
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            log::warn!("System color-scheme watcher stopped: {error}");
        }
    }
}
