use std::path::PathBuf;

/// Path of the app logo in the data directory (PNG for runtime decoding).
pub fn logo_path() -> Option<PathBuf> {
    let logo = crate::settings::data_dir().ok()?.join("NADT.png");
    if logo.exists() { Some(logo) } else { None }
}

/// Path of the app icon in the data directory (ICO for Windows shell usage).
pub fn icon_path() -> Option<PathBuf> {
    let icon = crate::settings::data_dir().ok()?.join("NADT.ico");
    if icon.exists() { Some(icon) } else { None }
}

/// Best icon for the toast source.
fn toast_icon() -> Option<PathBuf> {
    logo_path().or_else(icon_path)
}

/// Load the tray/window icon from `data/NADT.png`.
pub fn load_icon() -> Option<tauri::image::Image<'static>> {
    let img = image::open(logo_path()?) .ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    Some(tauri::image::Image::new_owned(img.into_raw(), w, h))
}

/// Load a dimmed copy for tray activity indication without changing the icon
/// shape or its occupied tray area.
pub fn load_dimmed_icon() -> Option<tauri::image::Image<'static>> {
    let mut img = image::open(logo_path()?).ok()?.to_rgba8();
    for pixel in img.pixels_mut() {
        pixel[0] = ((pixel[0] as f32) * 0.38) as u8;
        pixel[1] = ((pixel[1] as f32) * 0.38) as u8;
        pixel[2] = ((pixel[2] as f32) * 0.38) as u8;
    }
    let (w, h) = (img.width(), img.height());
    Some(tauri::image::Image::new_owned(img.into_raw(), w, h))
}

/// AppUserModelID this app registers for itself, under the plain `NADT` name.
///
/// Windows needs an entry under `AppUserModelId` to attribute a toast to an
/// application; without one it credits the host process (PowerShell). Only the
/// per-user registry is touched: no shortcut and no file is created anywhere.
pub const APP_ID: &str = "NADT";
/// Title shown on every toast.
pub const APP_NAME: &str = "NADT";

/// Register the AUMID key so Windows shows `NADT` as the notification source.
///
/// Per-user registration needs no administrator rights, and repeating it is
/// harmless. Failures are ignored: notifications still work, they just fall
/// back to naming the host process.
#[cfg(windows)]
pub fn register_app_id() {
    use std::os::windows::process::CommandExt;
    let icon = toast_icon().map(|p| p.display().to_string()).unwrap_or_default();
    let script = format!(
        r#"$ErrorActionPreference='SilentlyContinue'
$id = 'HKCU:\Software\Classes\AppUserModelId\{APP_ID}'
New-Item -Path $id -Force | Out-Null
Set-ItemProperty -Path $id -Name 'DisplayName' -Value '{APP_NAME}'
if ('{icon}') {{ Set-ItemProperty -Path $id -Name 'IconUri' -Value '{icon}' }}
$ns = 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Notifications\Settings\{APP_ID}'
New-Item -Path $ns -Force | Out-Null
Set-ItemProperty -Path $ns -Name 'Enabled' -Value 1 -Type DWord
"#,
        APP_ID = APP_ID,
        APP_NAME = APP_NAME,
        icon = icon,
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status();
}

#[cfg(not(windows))]
pub fn register_app_id() {}

/// Send a Windows toast notification.
///
/// Windows prints the notification source above the toast, and it takes that
/// name from the AUMID we registered, so the toast body must not repeat it:
/// the visible result is `NADT` once, followed by the event and the detail.
///
/// When `sound` is false the toast is created with `sound(None)`, which maps to
/// the `<audio silent="true"/>` payload and produces no system sound.
#[cfg(windows)]
pub fn send_notification(event: &str, detail: &str, sound: bool) {
    use winrt_notification::{Duration, Sound, Toast};
    let mut toast = Toast::new(APP_ID)
        .text1(event)
        .duration(Duration::Short)
        .sound(if sound { Some(Sound::Default) } else { None });
    if !detail.is_empty() {
        toast = toast.text2(detail);
    }
    let _ = toast.show();
}

#[cfg(not(windows))]
pub fn send_notification(event: &str, detail: &str, _sound: bool) {
    let _ = (event, detail);
}

