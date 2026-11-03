//! OS light/dark preference.
//!
//! WebKitGTK derives `prefers-color-scheme` from the GTK theme, which on KDE
//! (and other non-GTK desktops) does not follow the desktop's light/dark
//! setting. The XDG Settings portal is what browsers and GTK4 apps read
//! instead, so the frontend asks for it here. On other platforms the command
//! returns `None` and the frontend keeps using `prefers-color-scheme`.

use tauri::AppHandle;

pub const SYSTEM_COLOR_SCHEME_EVENT: &str = "system-color-scheme-changed";

/// "dark", "light", or `None` when the OS gives no preference.
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

/// Emit `system-color-scheme-changed` (payload "dark"/"light") on OS changes.
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
    fn label(value: u32) -> Option<String> {
        match value {
            1 => Some("dark".into()),
            2 => Some("light".into()),
            _ => None,
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
        label(u32::try_from(value).ok()?)
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
                // "No preference" is not a change the frontend can act on.
                if let Some(scheme) = u32::try_from(value).ok().and_then(label) {
                    let _ = app.emit(SYSTEM_COLOR_SCHEME_EVENT, scheme);
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
