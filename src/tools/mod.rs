//! Everything the assistant can do on the PC. Each tool is plain Rust calling
//! Windows APIs; nothing runs until the model (or a local intent) asks.
//!
//! Hard limits we design around:
//! * Windows blocks input into elevated (admin) windows, UAC prompts and the
//!   lock screen. We detect elevated targets and say so instead of failing
//!   silently. We never request elevation ourselves.
//! * No shell/terminal tool: arbitrary commands are deliberately not offered.
//! * Deletes go to the Recycle Bin, and need an on-screen "Allow".

pub mod apps;
pub mod files;
pub mod input;
pub mod media;
pub mod system;
pub mod uia;
pub mod volume;
pub mod wins;

use crate::events::{self, UiEvent};
use crate::util::WStr;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SW_SHOWNORMAL, SetForegroundWindow};
use windows::core::w;

/// COM for the current thread, released on drop.
pub struct ComGuard(bool);
impl ComGuard {
    pub fn mta() -> ComGuard {
        ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() })
    }
    pub fn sta() -> ComGuard {
        ComGuard(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() })
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

pub fn shell_open(target: &str) -> bool {
    let t = WStr::new(target);
    unsafe { ShellExecuteW(None, w!("open"), t.pcwstr(), None, None, SW_SHOWNORMAL).0 as isize > 32 }
}

// ---------------------------------------------------------------- the user's window

static USER_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Remember which window the user was in before the island took focus, so
/// "type this" or "close this" act on the right thing.
pub fn remember_user_window(island: HWND) {
    let fg = unsafe { GetForegroundWindow() };
    if !fg.is_invalid() && fg != island {
        USER_WINDOW.store(fg.0 as isize, Ordering::SeqCst);
    }
}

pub fn user_window() -> Option<HWND> {
    let h = HWND(USER_WINDOW.load(Ordering::SeqCst) as _);
    (!h.is_invalid() && unsafe { IsWindow(Some(h)).as_bool() }).then_some(h)
}

pub fn restore_user_window() {
    if let Some(h) = user_window() {
        wins::force_foreground(h);
    } else {
        unsafe {
            let _ = SetForegroundWindow(GetForegroundWindow());
        }
    }
}

fn process_elevated(h: HANDLE) -> Option<bool> {
    unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(h, TOKEN_QUERY, &mut tok).ok()?;
        let mut el = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let r = GetTokenInformation(tok, TokenElevation, Some(&mut el as *mut _ as *mut _), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        let _ = CloseHandle(tok);
        r.ok()?;
        Some(el.TokenIsElevated != 0)
    }
}

/// True when Windows will block us from sending input to this window
/// (it runs as administrator and we don't).
pub fn blocked_by_elevation(hwnd: HWND) -> bool {
    unsafe {
        if process_elevated(GetCurrentProcess()) == Some(true) {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return true };
        let elevated = process_elevated(p);
        let _ = CloseHandle(p);
        elevated.unwrap_or(true)
    }
}

pub const ELEVATED_MSG: &str = "That window runs as administrator, and Windows doesn't let other apps control it. Do this one by hand, or run OpenAssist as admin.";

// ---------------------------------------------------------------- confirmation

/// Shows Allow/Deny on the island and waits (max 45 s).
pub fn confirm(prompt: &str) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    events::post(UiEvent::Confirm(prompt.to_string(), tx));
    rx.recv_timeout(Duration::from_secs(45)).unwrap_or(false)
}

// ---------------------------------------------------------------- registry

fn tool(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {"type": "object", "properties": props, "required": required}
        }
    })
}

/// Tool schemas, kept terse: every token here is sent with each request.
pub fn schemas(allow_ui: bool) -> Value {
    let mut t = vec![
        tool("media", "Control whatever is playing (Spotify, browser, etc.).", json!({"action": {"type": "string", "enum": ["play", "pause", "toggle", "next", "previous", "now_playing"]}}), &["action"]),
        tool("play_music", "Search and open a song/artist/playlist in Spotify (falls back to YouTube Music if Spotify isn't installed).", json!({"query": {"type": "string"}}), &["query"]),
        tool("volume", "Set system volume.", json!({"level": {"type": "integer", "description": "0-100"}, "change": {"type": "integer", "description": "relative, e.g. -10"}, "mute": {"type": "boolean"}}), &[]),
        tool("open_app", "Launch an installed app by name.", json!({"name": {"type": "string"}}), &["name"]),
        tool("open", "Open a folder, file or URL. Folder names like downloads, documents, desktop, pictures, music, videos, home work; also sub-paths like 'downloads/invoices'.", json!({"target": {"type": "string"}}), &["target"]),
        tool("list_files", "List a folder's contents (newest first).", json!({"folder": {"type": "string"}, "pattern": {"type": "string", "description": "wildcard like *.pdf"}}), &["folder"]),
        tool("find_files", "Find files by name under a folder (searches 4 levels deep).", json!({"name": {"type": "string", "description": "part of name or wildcard"}, "folder": {"type": "string", "description": "default: home"}}), &["name"]),
        tool(
            "file_op",
            "Move, copy, rename, delete (to Recycle Bin) files, or make a folder. 'paths' accepts wildcards like 'downloads/*.pdf'.",
            json!({"op": {"type": "string", "enum": ["move", "copy", "rename", "delete", "mkdir"]}, "paths": {"type": "array", "items": {"type": "string"}}, "to": {"type": "string", "description": "destination folder, or new name for rename"}}),
            &["op", "paths"],
        ),
        tool("windows", "List or control app windows. target: part of a title/app name, or 'current'.", json!({"action": {"type": "string", "enum": ["list", "focus", "minimize", "maximize", "restore", "close", "snap_left", "snap_right", "minimize_all"]}, "target": {"type": "string"}}), &["action"]),
        tool("web_search", "Search the web in the browser.", json!({"query": {"type": "string"}, "site": {"type": "string", "enum": ["google", "youtube", "bing", "duckduckgo", "wikipedia", "amazon"]}}), &["query"]),
        tool(
            "system",
            "System actions: lock, dark_mode, light_mode, open_settings (with page like bluetooth, wifi, display, sound), empty_recycle_bin, screenshot (opens snipping), time.",
            json!({"action": {"type": "string", "enum": ["lock", "dark_mode", "light_mode", "open_settings", "empty_recycle_bin", "screenshot", "time"]}, "page": {"type": "string"}}),
            &["action"],
        ),
        tool("clipboard", "Read or set clipboard text.", json!({"action": {"type": "string", "enum": ["get", "set"]}, "text": {"type": "string"}}), &["action"]),
        tool("reminder", "Remind the user after some minutes (lasts while the app runs).", json!({"minutes": {"type": "number"}, "text": {"type": "string"}}), &["minutes", "text"]),
    ];
    if allow_ui {
        t.push(tool("ui_list", "List clickable controls/fields in a window (default: the user's current window). Use before ui_click when unsure of names.", json!({"window": {"type": "string"}}), &[]));
        t.push(tool("ui_click", "Click a button/menu/link/tab by its visible name in a window (default: current).", json!({"name": {"type": "string"}, "window": {"type": "string"}}), &["name"]));
        t.push(tool("type_text", "Type text into the current window, optionally into a named field first.", json!({"text": {"type": "string"}, "field": {"type": "string"}, "window": {"type": "string"}}), &["text"]));
        t.push(tool("press_keys", "Press a key combo in the current window, e.g. 'ctrl+s', 'enter', 'alt+tab', 'ctrl+shift+t'.", json!({"keys": {"type": "string"}}), &["keys"]));
    }
    Value::Array(t)
}

/// A short "what I'm doing" label for the island while a tool runs.
pub fn label(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match name {
        "media" => "Controlling playback…".into(),
        "play_music" => format!("Finding {}…", s("query")),
        "volume" => "Adjusting volume…".into(),
        "open_app" => format!("Opening {}…", s("name")),
        "open" => format!("Opening {}…", s("target")),
        "list_files" | "find_files" => "Looking through files…".into(),
        "file_op" => format!("{} files…", capitalize(&s("op"))),
        "windows" => "Arranging windows…".into(),
        "web_search" => format!("Searching {}…", s("query")),
        "ui_list" => "Reading the window…".into(),
        "ui_click" => format!("Clicking “{}”…", s("name")),
        "type_text" => "Typing…".into(),
        "press_keys" => format!("Pressing {}…", s("keys")),
        _ => "Working on it…".into(),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "Working on".into(),
    }
}

/// Runs a tool and returns a short text result for the model.
pub fn execute(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let r: Result<String, String> = match name {
        "media" => {
            let a = s("action");
            if a == "now_playing" { Ok(media::now_playing().describe()) } else { media::control(&a) }
        }
        "play_music" => media::play_music(&s("query")),
        "volume" => volume::tool(args),
        "open_app" => apps::open(&s("name")),
        "open" => files::open(&s("target")),
        "list_files" => files::list(&s("folder"), &s("pattern")),
        "find_files" => files::find(&s("name"), &s("folder")),
        "file_op" => files::op(args),
        "windows" => wins::tool(&s("action"), &s("target")),
        "web_search" => system::web_search(&s("query"), &s("site")),
        "system" => system::tool(&s("action"), &s("page")),
        "clipboard" => system::clipboard_tool(&s("action"), &s("text")),
        "reminder" => system::reminder(args.get("minutes").and_then(Value::as_f64).unwrap_or(0.0), &s("text")),
        "ui_list" => uia::list(&s("window")),
        "ui_click" => uia::click(&s("name"), &s("window")),
        "type_text" => uia::type_into(&s("text"), &s("field"), &s("window")),
        "press_keys" => input::press_combo_in(&s("keys"), &s("window")),
        _ => Err(format!("unknown tool {name}")),
    };
    match r {
        Ok(t) => crate::util::truncate(&t, 1800),
        Err(e) => format!("ERROR: {e}"),
    }
}
