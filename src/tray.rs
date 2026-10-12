//! Notification-area icon. The icon is painted at startup (an ember orb),
//! so the exe needs no resource files.

use crate::util::wide;
use std::sync::OnceLock;
use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

pub const WM_TRAY: u32 = WM_APP + 2;
const TRAY_ID: u32 = 1;

pub enum Cmd {
    Talk,
    Type,
    Menu,
    Settings,
    Quit,
}

impl Cmd {
    pub fn from(id: usize) -> Option<Cmd> {
        Some(match id {
            101 => Cmd::Talk,
            102 => Cmd::Type,
            103 => Cmd::Menu,
            104 => Cmd::Settings,
            199 => Cmd::Quit,
            _ => return None,
        })
    }
}

pub fn taskbar_created() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

fn icon() -> HICON {
    static ICON: OnceLock<isize> = OnceLock::new();
    HICON(*ICON.get_or_init(|| unsafe { make_icon().0 as isize }) as _)
}

pub fn add(hwnd: HWND) {
    let _ = taskbar_created();
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon(),
        ..Default::default()
    };
    let tip = wide(&format!("OpenAssist — {} to talk", crate::island::hotkeys().0));
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

pub fn remove(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

pub fn on_message(hwnd: HWND, lp: LPARAM) {
    match (lp.0 & 0xffff) as u32 {
        WM_LBUTTONUP => crate::island::get().on_tray_click(),
        WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
        _ => {}
    }
}

pub fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let (talk, typ) = crate::island::hotkeys();
        let talk = wide(&format!("Talk\t{talk}"));
        let typ = wide(&format!("Type a command\t{typ}"));
        let _ = AppendMenuW(menu, MF_STRING, 101, windows::core::PCWSTR(talk.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, 102, windows::core::PCWSTR(typ.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, 103, w!("Quick menu"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, 104, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, 199, w!("Quit OpenAssist"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

/// Paints a 32x32 ember orb into an ARGB icon.
unsafe fn make_icon() -> HICON {
    unsafe {
        let n = 32i32;
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0) else { return HICON::default() };
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (n * n) as usize);
        let c = (n as f32 - 1.0) / 2.0;
        for y in 0..n {
            for x in 0..n {
                let dx = x as f32 - c;
                let dy = y as f32 - c;
                let d = (dx * dx + dy * dy).sqrt() / 14.5;
                let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
                if d <= 1.0 {
                    // body: cream core -> ember -> orange rim
                    let hd = (((dx + 4.0).powi(2) + (dy + 4.0).powi(2)).sqrt() / 18.0).min(1.0);
                    let lerp = |p: f32, q: f32, t: f32| p + (q - p) * t;
                    r = lerp(255.0, 236.0, hd);
                    g = lerp(246.0, 150.0, hd);
                    b = lerp(214.0, 58.0, hd);
                    a = ((1.0 - d) * 14.5).clamp(0.0, 1.0);
                }
                // eyes
                for ex in [-4.5f32, 4.5] {
                    let ed = ((dx - ex) / 1.6).powi(2) + ((dy - 0.5) / 2.6).powi(2);
                    if ed <= 1.0 {
                        r = 42.0;
                        g = 28.0;
                        b = 6.0;
                    }
                }
                let a8 = (a * 255.0) as u32;
                let pm = |v: f32| ((v * a) as u32).min(255);
                px[(y * n + x) as usize] = a8 << 24 | pm(r) << 16 | pm(g) << 8 | pm(b);
            }
        }
        let mask = CreateBitmap(n, n, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
