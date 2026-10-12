//! Top-level window listing and control (focus, min/max, snap, close).

use crate::util::{from_wide, fuzzy_score};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_D, VK_LWIN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PWSTR};

pub struct Win {
    pub hwnd: HWND,
    pub title: String,
    pub app: String,
}

fn exe_name(hwnd: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(p);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or("").trim_end_matches(".exe").trim_end_matches(".EXE").to_string()
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let out = &mut *(lp.0 as *mut Vec<Win>);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        if GetWindow(hwnd, GW_OWNER).map(|o| !o.is_invalid()).unwrap_or(false) {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
        if cloaked != 0 {
            return true.into();
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n == 0 {
            return true.into();
        }
        let title = from_wide(&buf[..n as usize]);
        if title == "Program Manager" || title == "OpenAssist" {
            return true.into();
        }
        out.push(Win { hwnd, title, app: exe_name(hwnd) });
        true.into()
    }
}

pub fn list() -> Vec<Win> {
    let mut v: Vec<Win> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Finds a window by title or app name; "current"/"" = the user's window.
pub fn find(target: &str) -> Option<HWND> {
    let t = target.trim().to_lowercase();
    if t.is_empty() || matches!(t.as_str(), "current" | "this" | "this window" | "active" | "it") {
        return super::user_window().or_else(|| Some(unsafe { GetForegroundWindow() }));
    }
    list()
        .into_iter()
        .map(|w| (fuzzy_score(&w.title, &t).max(fuzzy_score(&w.app, &t) + 50), w.hwnd))
        .filter(|(s, _)| *s > 50)
        .max_by_key(|(s, _)| *s)
        .map(|(_, h)| h)
}

/// SetForegroundWindow is refused unless we "own" the last input; tapping
/// Alt first is the standard, harmless way to be allowed.
pub fn force_foreground(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if !SetForegroundWindow(h).as_bool() {
            super::input::tap_vk(windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU);
            let _ = SetForegroundWindow(h);
        }
    }
}

fn snap(h: HWND, left: bool) {
    unsafe {
        let _ = ShowWindow(h, SW_RESTORE);
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        // Compensate for the invisible resize borders around Win10/11 windows.
        let mut wr = RECT::default();
        let mut fr = RECT::default();
        let _ = GetWindowRect(h, &mut wr);
        let _ = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut fr as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32);
        let (bl, bt, br, bb) = (fr.left - wr.left, fr.top - wr.top, wr.right - fr.right, wr.bottom - fr.bottom);
        let half = (wa.right - wa.left) / 2;
        let x = if left { wa.left } else { wa.left + half };
        let _ = SetWindowPos(h, None, x - bl, wa.top - bt, half + bl + br, (wa.bottom - wa.top) + bt + bb, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub fn tool(action: &str, target: &str) -> Result<String, String> {
    if action == "list" {
        let ws = list();
        let lines: Vec<String> = ws.iter().take(30).map(|w| format!("{} — {}", w.app, crate::util::truncate(&w.title, 70))).collect();
        return Ok(format!("{} windows:\n{}", ws.len(), lines.join("\n")));
    }
    if action == "minimize_all" {
        super::input::press_combo(&[VK_LWIN, VK_D]);
        return Ok("Showed the desktop.".into());
    }
    let h = find(target).ok_or_else(|| format!("no window matching “{target}”"))?;
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    let title = crate::util::truncate(&from_wide(&buf[..n.max(0) as usize]), 60);
    unsafe {
        match action {
            "focus" => force_foreground(h),
            "minimize" => {
                let _ = ShowWindow(h, SW_MINIMIZE);
            }
            "maximize" => {
                let _ = ShowWindow(h, SW_MAXIMIZE);
            }
            "restore" => {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            "close" => {
                let _ = PostMessageW(Some(h), WM_CLOSE, Default::default(), Default::default());
            }
            "snap_left" => snap(h, true),
            "snap_right" => snap(h, false),
            _ => return Err(format!("unknown window action {action}")),
        }
    }
    Ok(format!("{action} → {title}"))
}
