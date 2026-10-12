//! System odds and ends: lock, theme, Settings pages, Recycle Bin, web
//! search, clipboard, reminders, and run-at-startup.

use crate::events::{self, UiEvent};
use crate::oauth::urlencode;
use crate::util::{WStr, wide};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Shutdown::LockWorkStation;
use windows::Win32::UI::Shell::{SHERB_NOSOUND, SHEmptyRecycleBinW};
use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
use windows::core::{PCWSTR, w};

const CF_UNICODETEXT: u32 = 13;

pub fn web_search(query: &str, site: &str) -> Result<String, String> {
    let q = urlencode(query);
    let url = match site {
        "youtube" => format!("https://www.youtube.com/results?search_query={q}"),
        "bing" => format!("https://www.bing.com/search?q={q}"),
        "duckduckgo" => format!("https://duckduckgo.com/?q={q}"),
        "wikipedia" => format!("https://en.wikipedia.org/w/index.php?search={q}"),
        "amazon" => format!("https://www.amazon.com/s?k={q}"),
        _ => format!("https://www.google.com/search?q={q}"),
    };
    super::shell_open(&url);
    Ok(format!("Opened search for “{query}”{}.", if site.is_empty() { String::new() } else { format!(" on {site}") }))
}

fn set_theme(light: bool) -> Result<(), String> {
    unsafe {
        let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let v: u32 = if light { 1 } else { 0 };
        let bytes = v.to_le_bytes();
        for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
            let r = RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some(bytes.as_ptr() as _), 4);
            if r.is_err() {
                return Err(format!("registry write failed ({})", r.0));
            }
        }
        let topic = wide("ImmersiveColorSet");
        let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(topic.as_ptr() as isize), SMTO_ABORTIFHUNG, 200, None);
    }
    Ok(())
}

pub fn tool(action: &str, page: &str) -> Result<String, String> {
    match action {
        "lock" => unsafe {
            LockWorkStation().map_err(|e| e.message())?;
            Ok("Locked.".into())
        },
        "dark_mode" => set_theme(false).map(|_| "Switched to dark mode.".into()),
        "light_mode" => set_theme(true).map(|_| "Switched to light mode.".into()),
        "open_settings" => {
            let p = page.trim().to_lowercase().replace(' ', "");
            let uri = match p.as_str() {
                "" => "ms-settings:".to_string(),
                "wifi" | "wi-fi" | "network" | "internet" => "ms-settings:network-wifi".into(),
                "bluetooth" | "devices" => "ms-settings:bluetooth".into(),
                "display" | "screen" | "brightness" => "ms-settings:display".into(),
                "sound" | "audio" => "ms-settings:sound".into(),
                "notifications" => "ms-settings:notifications".into(),
                "battery" | "power" => "ms-settings:batterysaver".into(),
                "apps" => "ms-settings:appsfeatures".into(),
                "update" | "windowsupdate" => "ms-settings:windowsupdate".into(),
                "wallpaper" | "background" | "personalization" => "ms-settings:personalization-background".into(),
                "privacy" | "microphone" => "ms-settings:privacy-microphone".into(),
                "storage" => "ms-settings:storagesense".into(),
                "mouse" => "ms-settings:mousetouchpad".into(),
                "keyboard" | "typing" => "ms-settings:typing".into(),
                "language" | "speech" => "ms-settings:speech".into(),
                "focus" | "donotdisturb" => "ms-settings:focus".into(),
                other => format!("ms-settings:{other}"),
            };
            super::shell_open(&uri);
            Ok(format!("Opened Settings ({}).", if page.is_empty() { "home" } else { page }))
        }
        "empty_recycle_bin" => {
            if !super::confirm("Permanently empty the Recycle Bin? This can't be undone.") {
                return Ok("The user declined.".into());
            }
            unsafe {
                let _ = SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOSOUND);
            }
            Ok("Emptied the Recycle Bin.".into())
        }
        "screenshot" => {
            super::shell_open("ms-screenclip:");
            Ok("Opened the snipping tool.".into())
        }
        "time" => Ok(now_string()),
        _ => Err(format!("unknown system action {action}")),
    }
}

pub fn now_string() -> String {
    unsafe {
        let st = windows::Win32::System::SystemInformation::GetLocalTime();
        let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        let months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        let (h12, ampm) = match st.wHour {
            0 => (12, "AM"),
            h if h < 12 => (h, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        format!(
            "{}, {} {}, {}, {}:{:02} {}",
            days[st.wDayOfWeek as usize % 7],
            months[(st.wMonth as usize).saturating_sub(1) % 12],
            st.wDay,
            st.wYear,
            h12,
            st.wMinute,
            ampm
        )
    }
}

// ---------------------------------------------------------------- clipboard

pub fn clipboard_get() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if !p.is_null() {
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                let _ = GlobalUnlock(HGLOBAL(h.0));
            }
        }
        let _ = CloseClipboard();
        out
    }
}

pub fn clipboard_set(text: &str) -> Result<(), String> {
    unsafe {
        let w = wide(text);
        let bytes = w.len() * 2;
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.message())?;
        let p = GlobalLock(h) as *mut u16;
        if p.is_null() {
            return Err("clipboard memory".into());
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        let _ = GlobalUnlock(h);
        OpenClipboard(None).map_err(|e| e.message())?;
        let _ = EmptyClipboard();
        let r = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0)));
        let _ = CloseClipboard();
        r.map(|_| ()).map_err(|e| e.message())
    }
}

pub fn clipboard_tool(action: &str, text: &str) -> Result<String, String> {
    match action {
        "set" => clipboard_set(text).map(|_| "Copied to clipboard.".into()),
        _ => Ok(clipboard_get().map(|t| crate::util::truncate(&t, 1500)).unwrap_or_else(|| "(clipboard has no text)".into())),
    }
}

// ---------------------------------------------------------------- reminders

pub fn reminder(minutes: f64, text: &str) -> Result<String, String> {
    if !(0.05..=24.0 * 60.0).contains(&minutes) {
        return Err("reminders can be from a few seconds up to 24 hours".into());
    }
    let t = text.to_string();
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(minutes * 60.0));
            events::post(UiEvent::Reminder(t));
        })
        .map_err(|e| e.to_string())?;
    let when = if minutes < 1.0 { format!("{} seconds", (minutes * 60.0).round()) } else { format!("{} minute(s)", minutes.round()) };
    Ok(format!("Reminder set for {when}."))
}

// ---------------------------------------------------------------- startup

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

pub fn run_at_startup() -> bool {
    unsafe {
        let mut size = 0u32;
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("OpenAssist"), RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok()
    }
}

pub fn set_run_at_startup(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().map(|p| format!("\"{}\"", p.display())).unwrap_or_default();
            let v = WStr::new(&exe);
            let w = wide(&exe);
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("OpenAssist"), REG_SZ.0, Some(v.pcwstr().0 as _), (w.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("OpenAssist"));
        }
    }
}
