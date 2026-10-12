//! Worker threads never touch the UI directly. They queue an event and nudge
//! the island window with a single PostMessage; the UI thread drains the queue.

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_UI_EVENT: u32 = WM_APP + 1;

pub enum UiEvent {
    /// Mic level 0..1 while recording.
    Level(f32),
    Transcribing,
    /// What we heard (or what was typed), shown under the orb.
    Heard(String),
    Thinking(String),
    Reply(String),
    Error(String),
    /// Ask the user to approve a risky action.
    Confirm(String, Sender<bool>),
    /// Recording ended with nothing usable.
    Idle,
    /// Settings changed off-thread (e.g. OAuth finished).
    ConfigChanged(String),
    Reminder(String),
    /// Now playing + master volume + mute, for the menu.
    Media(crate::tools::media::MediaInfo, f32, bool),
}

static QUEUE: Mutex<Vec<UiEvent>> = Mutex::new(Vec::new());
static HWND_ISLAND: AtomicIsize = AtomicIsize::new(0);

pub fn set_target(hwnd: HWND) {
    HWND_ISLAND.store(hwnd.0 as isize, Ordering::SeqCst);
}

pub fn post(ev: UiEvent) {
    QUEUE.lock().unwrap().push(ev);
    let h = HWND_ISLAND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_UI_EVENT, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn drain() -> Vec<UiEvent> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}
