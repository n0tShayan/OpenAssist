//! The island: a black-glass notch hanging from the top edge of the screen.
//! Asleep it is a 184x11 DIP lip that costs nothing (DWM composites the last
//! frame). It springs open to listen, think, reply, take typed commands, show
//! the quick menu, or edit settings.
//!
//! Frame timers only run while something is actually moving.

use crate::events::{self, UiEvent, WM_UI_EVENT};
use crate::gfx::*;
use crate::textbox::{Key, TextBox};
use crate::tools::media::MediaInfo;
use crate::{agent, audio, config, oauth, tools, tray, tts};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForMonitor;
use windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

// ---------------------------------------------------------------- palette

const GLASS_TOP: Color = rgba(0x161618, 0.985);
const GLASS_BOTTOM: Color = rgba(0x000000, 0.985);
const INK: Color = rgba(0xF4F3EE, 1.0);
const INK_DIM: Color = rgba(0xA3A19A, 1.0);
const INK_FAINT: Color = rgba(0x66645E, 1.0);
const EMBER: Color = rgba(0xF2C94C, 1.0);
const EMBER_HOT: Color = rgba(0xFF9E45, 1.0);
const EMBER_INK: Color = rgba(0x2A1C06, 1.0);
const CARD: Color = rgba(0xFFFFFF, 0.055);
const CARD_HOVER: Color = rgba(0xFFFFFF, 0.10);
const LINE: Color = rgba(0xFFFFFF, 0.08);
const GREEN: Color = rgba(0x63D68F, 1.0);

// Segoe Fluent Icons / MDL2 glyphs
const IC_PREV: char = '\u{E892}';
const IC_PLAY: char = '\u{E768}';
const IC_PAUSE: char = '\u{E769}';
const IC_NEXT: char = '\u{E893}';
const IC_VOL: char = '\u{E767}';
const IC_MUTE: char = '\u{E74F}';
const IC_MIC: char = '\u{E720}';
const IC_KEYBOARD: char = '\u{E765}';
const IC_SETTINGS: char = '\u{E713}';
const IC_POWER: char = '\u{E7E8}';
const IC_CLOSE: char = '\u{E711}';
const IC_MUSIC: char = '\u{EC4F}';
const IC_LINK: char = '\u{E8A7}';
const IC_DELETE: char = '\u{E74D}';
const IC_CHECK: char = '\u{E73E}';

// ---------------------------------------------------------------- timers

const T_FRAME: usize = 1;
const T_REC: usize = 3;
const T_DISMISS: usize = 4;
const T_LEAVE: usize = 5;
const T_RELEASE: usize = 6;

const HK_TALK: i32 = 1;
const HK_CONSOLE: i32 = 2;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Sleep,
    Listening,
    Working,
    Reply,
    Console,
    Menu,
    Settings,
    Confirm,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Lip,
    MediaPrev,
    MediaToggle,
    MediaNext,
    VolMute,
    VolTrack,
    OpenConsole,
    Talk,
    OpenSettings,
    Quit,
    Close,
    ConnectOpenRouter,
    DisconnectOpenRouter,
    GetKey(usize),
    ClearKey(usize),
    Field(usize),
    Toggle(usize),
    Save,
    Allow,
    Deny,
    ConsoleField,
    DismissReply,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Shape {
    w: f32,
    d: f32,
    r: f32,
    rw: f32,
    rh: f32,
}

impl Shape {
    const fn new(w: f32, d: f32, r: f32, rw: f32, rh: f32) -> Shape {
        Shape { w, d, r, rw, rh }
    }
    fn lerp(a: Shape, b: Shape, t: f32) -> Shape {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Shape { w: l(a.w, b.w), d: l(a.d, b.d), r: l(a.r, b.r), rw: l(a.rw, b.rw), rh: l(a.rh, b.rh) }
    }
}

struct Anim {
    start: Instant,
    dur: f32,
    from: Shape,
    to: Shape,
    spring: bool,
}

fn ease_out_quart(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(4)
}

/// Soft spring: overshoots ~4% then settles (like Hitodama's island).
fn ease_spring(t: f32) -> f32 {
    let c1 = 1.15;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

const PAD_X: f32 = 40.0;
const PAD_Y: f32 = 34.0;
const BAR_PATTERN: [f32; 7] = [0.35, 0.62, 0.85, 1.0, 0.85, 0.62, 0.35];

pub struct Island {
    hwnd: HWND,
    g: Option<Gfx>,
    view: View,
    shape: Shape,
    anim: Option<Anim>,
    regions: Vec<(RectF, Act)>,
    hovered: Option<Act>,
    hover_lip: bool,
    tracking_leave: bool,
    mouse_down_track: bool,
    activatable: bool,
    // monitor & placement
    mon: RECT,
    scale: f32,
    hidden_fullscreen: bool,
    // continuous animation clock
    t0: Instant,
    caret_on: bool,
    // talking
    recording: Option<audio::Recording>,
    rec_started: Instant,
    rec_tap: bool,
    level: f32,
    level_smooth: f32,
    spoken: bool,
    // content
    heard: String,
    status: String,
    reply: String,
    reply_error: bool,
    console: TextBox,
    history: Vec<String>,
    hist_idx: usize,
    fields: [TextBox; 2], // gemini, groq
    focus_field: Option<usize>,
    toast: String,
    run_at_startup: bool,
    clip_listening: bool,
    confirm: Option<(String, Sender<bool>)>,
    media: MediaInfo,
    volume: f32,
    muted: bool,
}

static mut ISLAND: *mut Island = std::ptr::null_mut();

/// (talk, type) shortcut labels actually registered.
pub static HOTKEYS: std::sync::Mutex<(&str, &str)> = std::sync::Mutex::new(("Alt+Space", "Alt+Shift+Space"));

pub fn hotkeys() -> (&'static str, &'static str) {
    *HOTKEYS.lock().unwrap()
}

pub fn get() -> &'static mut Island {
    unsafe { &mut *ISLAND }
}

pub fn create() -> windows::core::Result<HWND> {
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = w!("OpenAssistIsland");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hwnd = CreateWindowExW(ex, class, w!("OpenAssist"), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None)?;

        let island = Box::new(Island {
            hwnd,
            g: Some(Gfx::new()?),
            view: View::Sleep,
            shape: Shape::default(),
            anim: None,
            regions: Vec::new(),
            hovered: None,
            hover_lip: false,
            tracking_leave: false,
            mouse_down_track: false,
            activatable: false,
            mon: RECT::default(),
            scale: 1.0,
            hidden_fullscreen: false,
            t0: Instant::now(),
            caret_on: true,
            recording: None,
            rec_started: Instant::now(),
            rec_tap: false,
            level: 0.0,
            level_smooth: 0.0,
            spoken: false,
            heard: String::new(),
            status: String::new(),
            reply: String::new(),
            reply_error: false,
            console: TextBox::default(),
            history: Vec::new(),
            hist_idx: 0,
            fields: [TextBox::masked(), TextBox::masked()],
            focus_field: None,
            toast: String::new(),
            run_at_startup: false,
            clip_listening: false,
            confirm: None,
            media: MediaInfo::default(),
            volume: 0.5,
            muted: false,
        });
        ISLAND = Box::into_raw(island);
        events::set_target(hwnd);

        let me = get();
        me.update_monitor();
        me.shape = me.target_shape(View::Sleep);
        me.present();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        me.register_hotkeys();
        tray::add(hwnd);
        me.bring_to_top();
        Ok(hwnd)
    }
}

impl Island {
    fn gfx(&mut self) -> &mut Gfx {
        self.g.as_mut().expect("gfx in use")
    }

    // ------------------------------------------------------------ setup

    /// Tries each shortcut in turn; another app (PowerToys Run, a launcher)
    /// may already own Alt+Space. All combos end in Space, which is what the
    /// hold-to-talk poll watches.
    fn register_hotkeys(&mut self) {
        let talk = [(MOD_ALT, "Alt+Space"), (MOD_CONTROL | MOD_ALT, "Ctrl+Alt+Space"), (MOD_CONTROL | MOD_SHIFT, "Ctrl+Shift+Space")];
        let console = [(MOD_ALT | MOD_SHIFT, "Alt+Shift+Space"), (MOD_CONTROL | MOD_ALT | MOD_SHIFT, "Ctrl+Alt+Shift+Space")];
        let reg = |id: i32, list: &[(HOT_KEY_MODIFIERS, &'static str)]| {
            list.iter()
                .find(|(m, _)| unsafe { RegisterHotKey(Some(self.hwnd), id, *m | MOD_NOREPEAT, VK_SPACE.0 as u32).is_ok() })
                .map(|(_, l)| *l)
        };
        let t = reg(HK_TALK, &talk);
        let c = reg(HK_CONSOLE, &console);
        *HOTKEYS.lock().unwrap() = (t.unwrap_or("tray icon"), c.unwrap_or("tray icon"));
        crate::util::log(&format!("hotkeys: talk={t:?} type={c:?}"));
        if t.is_none() || c.is_none() {
            self.show_reply("Every talk shortcut I tried is taken by other apps. Use the tray icon for now.".into(), true);
        }
    }

    fn update_monitor(&mut self) {
        unsafe {
            let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            self.mon = mi.rcMonitor;
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            self.scale = dx as f32 / 96.0;
            self.gfx().scale = self.scale;
        }
    }

    // ------------------------------------------------------------ view state

    fn target_shape(&mut self, v: View) -> Shape {
        match v {
            View::Sleep => {
                if self.hover_lip {
                    Shape::new(224.0, 26.0, 13.0, 11.0, 9.0)
                } else {
                    Shape::new(184.0, 11.0, 10.0, 8.0, 6.0)
                }
            }
            View::Listening => Shape::new(300.0, 56.0, 22.0, 16.0, 16.0),
            View::Working => Shape::new(340.0, if self.heard.is_empty() { 56.0 } else { 82.0 }, 22.0, 16.0, 16.0),
            View::Reply => {
                let w = 460.0;
                let reply = self.reply.clone();
                let (_, h) = self.gfx().measure(&reply, Font::Text, 14.0, 400, w - 52.0 - 26.0);
                Shape::new(w, (h + 38.0).clamp(60.0, 320.0), 26.0, 17.0, 18.0)
            }
            View::Console => Shape::new(580.0, 62.0, 24.0, 17.0, 18.0),
            View::Menu => Shape::new(468.0, 272.0, 30.0, 18.0, 20.0),
            View::Settings => Shape::new(580.0, 556.0, 32.0, 18.0, 20.0),
            View::Confirm => {
                let w = 470.0;
                let p = self.confirm.as_ref().map(|c| c.0.clone()).unwrap_or_default();
                let (_, h) = self.gfx().measure(&p, Font::Text, 13.5, 400, w - 82.0);
                Shape::new(w, (h + 112.0).clamp(146.0, 320.0), 28.0, 17.0, 18.0)
            }
        }
    }

    fn animate_to(&mut self, to: Shape) {
        let from = self.current_shape();
        if from == to {
            return;
        }
        let grow = to.w * to.d > from.w * from.d;
        let big = (to.d - from.d).abs() > 60.0;
        self.anim = Some(Anim {
            start: Instant::now(),
            dur: if big { 0.46 } else { 0.34 },
            from,
            to,
            spring: grow,
        });
        self.start_frames();
    }

    fn current_shape(&self) -> Shape {
        match &self.anim {
            Some(a) => {
                let t = (a.start.elapsed().as_secs_f32() / a.dur).min(1.0);
                let e = if a.spring { ease_spring(t) } else { ease_out_quart(t) };
                Shape::lerp(a.from, a.to, e)
            }
            None => self.shape,
        }
    }

    /// 0..1 fade for the content of the current view.
    fn content_alpha(&self) -> f32 {
        match &self.anim {
            Some(a) => {
                let t = a.start.elapsed().as_secs_f32() / a.dur;
                ease_out_quart(((t - 0.28) / 0.72).clamp(0.0, 1.0))
            }
            None => 1.0,
        }
    }

    fn set_view(&mut self, v: View) {
        let prev = self.view;
        self.view = v;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_DISMISS);
            let _ = KillTimer(Some(self.hwnd), T_LEAVE);
        }
        if prev == View::Settings && v != View::Settings {
            self.stop_clip_listener();
            self.focus_field = None;
        }
        let wants_focus = matches!(v, View::Console | View::Settings | View::Confirm);
        self.set_activatable(wants_focus);
        // The caret is drawn steady: a blink timer would repaint the whole
        // panel twice a second for nothing.
        if v == View::Reply {
            let ms = (3500 + self.reply.chars().count() as u32 * 55).min(20_000);
            unsafe {
                let _ = SetTimer(Some(self.hwnd), T_DISMISS, ms, None);
            }
        }
        crate::util::log(&format!("view {prev:?} -> {v:?}"));
        if v != View::Sleep {
            self.bring_to_top();
        }
        if v != View::Sleep && self.hidden_fullscreen {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        crate::efficiency_mode(v == View::Sleep);
        if v == View::Sleep {
            self.heard.clear();
            self.hovered = None;
            if self.hidden_fullscreen {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
        }
        if v == View::Menu {
            self.refresh_media();
        }
        let to = self.target_shape(v);
        self.animate_to(to);
        self.shape = to;
        self.present();
    }

    /// Other always-on-top windows can cover us; reclaim the top of the
    /// topmost band without activating.
    fn bring_to_top(&self) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        }
    }

    fn set_activatable(&mut self, on: bool) {
        if on == self.activatable {
            if on {
                unsafe {
                    let _ = SetForegroundWindow(self.hwnd);
                    let _ = SetFocus(Some(self.hwnd));
                }
            }
            return;
        }
        self.activatable = on;
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let ex = if on { ex & !(WS_EX_NOACTIVATE.0 as isize) } else { ex | WS_EX_NOACTIVATE.0 as isize };
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex);
            if on {
                // Releasing a held Alt after our hotkey can otherwise steal focus back.
                let _ = SetForegroundWindow(self.hwnd);
                let _ = SetFocus(Some(self.hwnd));
            } else if GetForegroundWindow() == self.hwnd {
                tools::restore_user_window();
            }
        }
    }

    fn start_frames(&self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_RELEASE);
            let _ = SetTimer(Some(self.hwnd), T_FRAME, 16, None);
        }
    }

    fn needs_frames(&self) -> bool {
        self.anim.is_some() || matches!(self.view, View::Listening | View::Working)
    }

    pub fn show_reply(&mut self, text: String, error: bool) {
        self.reply = text;
        self.reply_error = error;
        self.set_view(View::Reply);
    }

    // ------------------------------------------------------------ talking

    fn talk(&mut self) {
        if self.view == View::Confirm {
            return;
        }
        tts::stop();
        if self.recording.is_some() {
            self.stop_recording();
            return;
        }
        if agent::is_busy() {
            self.show_reply("Still working on the last request…".into(), false);
            return;
        }
        let cfg = config::get();
        if !cfg.can_hear() {
            if !cfg.has_any_key() {
                self.open_settings("Add a free Gemini or Groq key to talk to me.");
            } else {
                self.show_reply(format!("Voice needs a free Gemini or Groq key (OpenRouter can't hear). Type with {} instead.", hotkeys().1), true);
            }
            return;
        }
        suppress_alt_menu();
        tools::remember_user_window(self.hwnd);
        match audio::start() {
            Ok(rec) => {
                self.recording = Some(rec);
                self.rec_started = Instant::now();
                self.rec_tap = false;
                self.level = 0.0;
                self.spoken = true;
                self.heard.clear();
                self.set_view(View::Listening);
                unsafe {
                    let _ = SetTimer(Some(self.hwnd), T_REC, 30, None);
                }
            }
            Err(e) => self.show_reply(format!("Can't open the microphone: {e}"), true),
        }
    }

    fn poll_talk_key(&mut self) {
        let space_down = unsafe { GetAsyncKeyState(VK_SPACE.0 as i32) } < 0;
        if space_down {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_REC);
        }
        if self.rec_started.elapsed() < Duration::from_millis(350) {
            // A quick tap: hands-free mode, stop when the user pauses.
            self.rec_tap = true;
            if let Some(r) = &self.recording {
                r.set_auto_stop(true);
            }
            self.present();
        } else {
            self.stop_recording();
        }
    }

    fn stop_recording(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_REC);
        }
        if let Some(r) = self.recording.take() {
            r.stop();
            self.status = "Transcribing…".into();
            self.set_view(View::Working);
        }
    }

    fn open_console(&mut self) {
        if self.view == View::Confirm {
            return;
        }
        tools::remember_user_window(self.hwnd);
        self.spoken = false;
        self.console.clear();
        self.hist_idx = self.history.len();
        self.set_view(View::Console);
    }

    fn submit_console(&mut self) {
        let text = self.console.text().trim().to_string();
        if text.is_empty() {
            self.set_view(View::Sleep);
            return;
        }
        if !config::get().has_any_key() && !crate::intents::looks_local(&text) {
            self.open_settings("Connect a free AI provider first. Simple commands like “next song” work without one.");
            return;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
            if self.history.len() > 30 {
                self.history.remove(0);
            }
        }
        self.heard = text.clone();
        self.status = "Thinking…".into();
        self.set_view(View::Working);
        if !agent::submit_text(text, false) {
            self.show_reply("Still working on the last request…".into(), false);
        }
    }

    fn open_settings(&mut self, toast: &str) {
        self.toast = toast.to_string();
        let cfg = config::get();
        self.fields[0].clear();
        self.fields[1].clear();
        self.run_at_startup = tools::system::run_at_startup();
        self.focus_field = None;
        self.sniff_clipboard();
        let _ = cfg;
        self.set_view(View::Settings);
    }

    fn save_settings(&mut self) {
        let g = self.fields[0].text().trim().to_string();
        let q = self.fields[1].text().trim().to_string();
        let mut notes = Vec::new();
        if !g.is_empty() && !g.starts_with("AIza") {
            notes.push("That Gemini key looks unusual (they start with AIza) — saved anyway.");
        }
        if !q.is_empty() && !q.starts_with("gsk_") {
            notes.push("That Groq key looks unusual (they start with gsk_) — saved anyway.");
        }
        config::update(|c| {
            if !g.is_empty() {
                c.gemini_key = g.clone();
            }
            if !q.is_empty() {
                c.groq_key = q.clone();
            }
        });
        self.fields[0].clear();
        self.fields[1].clear();
        self.focus_field = None;
        self.toast = if notes.is_empty() { "Saved ✓".into() } else { notes.join(" ") };
        self.present();
    }

    fn sniff_clipboard(&mut self) {
        let Some(clip) = tools::system::clipboard_get() else { return };
        let clip = clip.trim();
        if clip.len() > 200 || clip.contains(char::is_whitespace) {
            return;
        }
        let cfg = config::get();
        if clip.starts_with("AIza") && cfg.gemini_key != clip {
            self.fields[0].set(clip);
            self.toast = "Gemini key found on your clipboard — press Save.".into();
        } else if clip.starts_with("gsk_") && cfg.groq_key != clip {
            self.fields[1].set(clip);
            self.toast = "Groq key found on your clipboard — press Save.".into();
        }
    }

    fn start_clip_listener(&mut self) {
        if !self.clip_listening {
            unsafe {
                let _ = AddClipboardFormatListener(self.hwnd);
            }
            self.clip_listening = true;
        }
    }

    fn stop_clip_listener(&mut self) {
        if self.clip_listening {
            unsafe {
                let _ = RemoveClipboardFormatListener(self.hwnd);
            }
            self.clip_listening = false;
        }
    }

    fn refresh_media(&self) {
        std::thread::spawn(|| {
            let _com = tools::ComGuard::mta();
            let info = tools::media::now_playing();
            let (vol, muted) = tools::volume::get().unwrap_or((0.5, false));
            events::post(UiEvent::Media(info, vol, muted));
        });
    }

    // ------------------------------------------------------------ actions

    fn act(&mut self, a: Act, x: f32) {
        match a {
            Act::Lip => self.set_view(View::Menu),
            Act::MediaPrev | Act::MediaToggle | Act::MediaNext => {
                let cmd = match a {
                    Act::MediaPrev => "previous",
                    Act::MediaNext => "next",
                    _ => "toggle",
                };
                if a == Act::MediaToggle {
                    self.media.playing = !self.media.playing;
                    self.present();
                }
                std::thread::spawn(move || {
                    let _com = tools::ComGuard::mta();
                    let _ = tools::media::control(cmd);
                    std::thread::sleep(Duration::from_millis(350));
                    let info = tools::media::now_playing();
                    let (vol, muted) = tools::volume::get().unwrap_or((0.5, false));
                    events::post(UiEvent::Media(info, vol, muted));
                });
            }
            Act::VolMute => {
                self.muted = !self.muted;
                let m = self.muted;
                std::thread::spawn(move || {
                    let _com = tools::ComGuard::mta();
                    let _ = tools::volume::set_mute(m);
                });
                self.present();
            }
            Act::VolTrack => {
                if let Some((r, _)) = self.regions.iter().find(|(_, a)| *a == Act::VolTrack) {
                    let v = ((x - r.x) / r.w).clamp(0.0, 1.0);
                    self.volume = v;
                    self.muted = false;
                    std::thread::spawn(move || {
                        let _com = tools::ComGuard::mta();
                        let _ = tools::volume::set(v);
                        let _ = tools::volume::set_mute(false);
                    });
                    self.present();
                }
            }
            Act::OpenConsole => self.open_console(),
            Act::Talk => {
                self.set_view(View::Sleep);
                self.talk();
            }
            Act::OpenSettings => self.open_settings(""),
            Act::Quit => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            Act::Close | Act::DismissReply => self.set_view(View::Sleep),
            Act::ConnectOpenRouter => {
                self.toast = "Finish signing in to OpenRouter in your browser…".into();
                self.present();
                oauth::connect_openrouter();
            }
            Act::DisconnectOpenRouter => {
                config::update(|c| {
                    c.openrouter_key.clear();
                    c.openrouter_model.clear();
                });
                self.toast = "OpenRouter disconnected.".into();
                self.present();
            }
            Act::GetKey(i) => {
                let url = if i == 0 { "https://aistudio.google.com/api-keys" } else { "https://console.groq.com/keys" };
                tools::shell_open(url);
                self.toast = "Create a key, copy it — I'll pick it up from the clipboard.".into();
                self.start_clip_listener();
                self.present();
            }
            Act::ClearKey(i) => {
                config::update(|c| {
                    if i == 0 {
                        c.gemini_key.clear()
                    } else {
                        c.groq_key.clear()
                    }
                });
                self.toast = if i == 0 { "Gemini key removed.".into() } else { "Groq key removed.".into() };
                self.present();
            }
            Act::Field(i) => {
                self.focus_field = Some(i);
                self.caret_on = true;
                self.present();
            }
            Act::Toggle(i) => {
                match i {
                    0 => config::update(|c| c.voice_replies = !c.voice_replies),
                    1 => config::update(|c| c.hide_in_fullscreen = !c.hide_in_fullscreen),
                    2 => config::update(|c| c.allow_ui_control = !c.allow_ui_control),
                    _ => {
                        self.run_at_startup = !self.run_at_startup;
                        tools::system::set_run_at_startup(self.run_at_startup);
                    }
                }
                self.present();
            }
            Act::Save => self.save_settings(),
            Act::Allow | Act::Deny => {
                if let Some((_, tx)) = self.confirm.take() {
                    let _ = tx.send(a == Act::Allow);
                }
                self.status = if a == Act::Allow { "Working on it…".into() } else { "Cancelled".into() };
                self.set_view(View::Working);
            }
            Act::ConsoleField => {}
        }
    }

    // ------------------------------------------------------------ events

    fn on_events(&mut self) {
        for ev in events::drain() {
            match ev {
                UiEvent::Level(l) => {
                    self.level = l;
                }
                UiEvent::Transcribing => {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), T_REC);
                    }
                    self.recording = None;
                    self.status = "Transcribing…".into();
                    self.set_view(View::Working);
                }
                UiEvent::Heard(t) => {
                    self.heard = t;
                    self.status = "Thinking…".into();
                    self.set_view(View::Working);
                }
                UiEvent::Thinking(s) => {
                    self.status = s;
                    if self.view != View::Confirm {
                        self.set_view(View::Working);
                    }
                }
                UiEvent::Reply(t) => {
                    if self.spoken && config::get().voice_replies {
                        tts::speak(&t);
                    }
                    self.show_reply(t, false);
                }
                UiEvent::Error(t) => self.show_reply(t, true),
                UiEvent::Confirm(p, tx) => {
                    if let Some((_, old)) = self.confirm.take() {
                        let _ = old.send(false);
                    }
                    self.confirm = Some((p, tx));
                    self.set_view(View::Confirm);
                }
                UiEvent::Idle => {
                    self.recording = None;
                    if matches!(self.view, View::Listening | View::Working) {
                        self.set_view(View::Sleep);
                    }
                }
                UiEvent::ConfigChanged(msg) => {
                    if self.view == View::Settings {
                        self.toast = msg;
                        self.present();
                    } else {
                        self.show_reply(msg, false);
                    }
                }
                UiEvent::Reminder(t) => {
                    tts::speak(&format!("Reminder: {t}"));
                    self.show_reply(format!("⏰  {t}"), false);
                }
                UiEvent::Media(info, vol, muted) => {
                    self.media = info;
                    self.volume = vol;
                    self.muted = muted;
                    if self.view == View::Menu {
                        self.present();
                    }
                }
            }
        }
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            T_FRAME => {
                if let Some(a) = &self.anim {
                    if a.start.elapsed().as_secs_f32() >= a.dur {
                        self.shape = a.to;
                        self.anim = None;
                    }
                }
                self.present();
                if !self.needs_frames() {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), T_FRAME);
                        let _ = SetTimer(Some(self.hwnd), T_RELEASE, 1500, None);
                    }
                }
            }
            T_REC => self.poll_talk_key(),
            T_DISMISS => {
                if self.view == View::Reply && !self.hovering() {
                    self.set_view(View::Sleep);
                }
            }
            T_LEAVE => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), T_LEAVE);
                }
                if self.view == View::Menu && !self.hovering() {
                    self.set_view(View::Sleep);
                }
            }
            T_RELEASE => unsafe {
                let _ = KillTimer(Some(self.hwnd), T_RELEASE);
                if !self.needs_frames() {
                    self.gfx().release();
                    if self.view == View::Sleep {
                        crate::trim_memory();
                    }
                }
            },
            _ => {}
        }
    }

    fn hovering(&self) -> bool {
        unsafe {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let mut r = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut r);
            p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if !self.tracking_leave {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking_leave = true;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_LEAVE);
        }
        if self.mouse_down_track {
            self.act(Act::VolTrack, x);
            return;
        }
        let hit = self.hit(x, y);
        if self.view == View::Sleep && !self.hover_lip {
            self.hover_lip = true;
            let to = self.target_shape(View::Sleep);
            self.animate_to(to);
            self.shape = to;
        }
        if hit != self.hovered {
            self.hovered = hit;
            if self.anim.is_none() {
                self.present();
            }
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking_leave = false;
        self.hovered = None;
        if self.hover_lip {
            self.hover_lip = false;
            if self.view == View::Sleep {
                let to = self.target_shape(View::Sleep);
                self.animate_to(to);
                self.shape = to;
            }
        }
        if self.view == View::Menu {
            unsafe {
                let _ = SetTimer(Some(self.hwnd), T_LEAVE, 700, None);
            }
        }
        if self.anim.is_none() {
            self.present();
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Act> {
        self.regions.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, a)| *a)
    }

    fn on_click(&mut self, x: f32, y: f32) {
        match self.hit(x, y) {
            Some(Act::VolTrack) => {
                self.mouse_down_track = true;
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.act(Act::VolTrack, x);
            }
            Some(a) => self.act(a, x),
            None => {
                if self.view == View::Sleep {
                    self.set_view(View::Menu);
                } else if self.view == View::Reply {
                    self.set_view(View::Sleep);
                } else if self.view == View::Settings {
                    self.focus_field = None;
                    self.present();
                }
            }
        }
    }

    fn on_key(&mut self, vk: VIRTUAL_KEY) {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match self.view {
            View::Confirm => match vk {
                VK_RETURN => self.act(Act::Allow, 0.0),
                VK_ESCAPE => self.act(Act::Deny, 0.0),
                _ => {}
            },
            View::Console => {
                match vk {
                    VK_ESCAPE => self.set_view(View::Sleep),
                    VK_RETURN => self.submit_console(),
                    VK_UP => {
                        if self.hist_idx > 0 {
                            self.hist_idx -= 1;
                            let h = self.history[self.hist_idx].clone();
                            self.console.set(&h);
                        }
                    }
                    VK_DOWN => {
                        if self.hist_idx + 1 < self.history.len() {
                            self.hist_idx += 1;
                            let h = self.history[self.hist_idx].clone();
                            self.console.set(&h);
                        } else {
                            self.hist_idx = self.history.len();
                            self.console.clear();
                        }
                    }
                    _ => edit_key(&mut self.console, vk, ctrl),
                }
                self.caret_on = true;
                if self.view == View::Console {
                    self.present();
                }
            }
            View::Settings => {
                match vk {
                    VK_ESCAPE => {
                        if self.focus_field.is_some() {
                            self.focus_field = None;
                        } else {
                            self.set_view(View::Sleep);
                            return;
                        }
                    }
                    VK_RETURN => self.save_settings(),
                    VK_TAB => self.focus_field = Some(match self.focus_field { Some(0) => 1, _ => 0 }),
                    _ => {
                        if let Some(i) = self.focus_field {
                            edit_key(&mut self.fields[i], vk, ctrl);
                        }
                    }
                }
                self.caret_on = true;
                self.present();
            }
            _ => {
                if vk == VK_ESCAPE {
                    self.set_view(View::Sleep);
                }
            }
        }
    }

    fn on_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        let s = ch.to_string();
        match self.view {
            View::Console => self.console.insert(&s),
            View::Settings => match self.focus_field {
                Some(i) => self.fields[i].insert(&s),
                None => return,
            },
            _ => return,
        }
        self.caret_on = true;
        self.present();
    }

    fn on_foreground_change(&mut self) {
        crate::util::log("foreground changed");
        self.bring_to_top();
        if !config::get().hide_in_fullscreen {
            return;
        }
        let fullscreen = unsafe {
            match SHQueryUserNotificationState() {
                Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
                Err(_) => false,
            }
        };
        if fullscreen != self.hidden_fullscreen {
            self.hidden_fullscreen = fullscreen;
            if self.view == View::Sleep {
                unsafe {
                    let _ = ShowWindow(self.hwnd, if fullscreen { SW_HIDE } else { SW_SHOWNOACTIVATE });
                }
            }
        }
    }

    // ------------------------------------------------------------ rendering

    fn present(&mut self) {
        let cur = self.current_shape();
        let (mw, md, mrw) = match &self.anim {
            Some(a) => (a.from.w.max(a.to.w).max(cur.w), a.from.d.max(a.to.d).max(cur.d), a.from.rw.max(a.to.rw)),
            None => (cur.w, cur.d, cur.rw),
        };
        let ww = mw + 2.0 * mrw + 2.0 * PAD_X;
        let wh = md + PAD_Y;
        let s = self.scale;
        let wpx = (ww * s).ceil() as i32;
        let hpx = (wh * s).ceil() as i32;
        let mon_w = self.mon.right - self.mon.left;
        let x = self.mon.left + (mon_w - wpx) / 2;
        let y = self.mon.top;
        let cx = (wpx as f32 / s) / 2.0;
        let alpha = self.content_alpha();
        let hwnd = self.hwnd;

        // Rendering borrows the Gfx mutably; move the state we need out first.
        let Some(mut g) = self.g.take() else { return };
        let t_render = Instant::now();
        self.regions.clear();
        let res = g.frame(hwnd, x, y, wpx, hpx, |c| {
            self.draw(c, cx, cur, alpha);
        });
        self.g = Some(g);
        if res.is_err() {
            crate::util::log(&format!("render failed: {res:?}"));
        }
        crate::util::log(&format!("present {:?} {}x{} in {:.1} ms", self.view, wpx, hpx, t_render.elapsed().as_secs_f32() * 1000.0));
        if !self.needs_frames() {
            // Static frame: DWM keeps showing it, so drop our copy soon.
            unsafe {
                let _ = SetTimer(Some(self.hwnd), T_RELEASE, 1200, None);
            }
        }
    }

    fn draw(&mut self, c: &mut Canvas, cx: f32, s: Shape, a: f32) {
        let f = c.g.d2d.clone();
        let Ok(geo) = notch_geometry(&f, cx, s.w, s.d, s.r, s.rw, s.rh) else { return };
        let geo: windows::Win32::Graphics::Direct2D::ID2D1Geometry = geo.into();

        // soft drop shadow
        if s.d > 14.0 {
            c.translate(0.0, 4.0);
            for i in 0..6 {
                c.stroke_geometry(&geo, rgba(0x000000, 0.045), 2.0 + i as f32 * 4.5);
            }
            c.translate(0.0, 0.0);
        }
        c.fill_geometry_vgradient(&geo, 0.0, s.d, GLASS_TOP, GLASS_BOTTOM);
        if s.d > 30.0 {
            c.stroke_geometry(&geo, rgba(0xFFFFFF, 0.075), 1.0);
        }

        let x0 = cx - s.w / 2.0;
        if self.view == View::Sleep {
            self.regions.push((RectF::new(x0 - s.rw, 0.0, s.w + 2.0 * s.rw, s.d.max(8.0)), Act::Lip));
            if self.hover_lip && s.d > 18.0 {
                let t = self.t0.elapsed().as_secs_f32();
                draw_orb(c, cx, s.d * 0.5 - 1.0, 6.5 * (s.d / 26.0), Mood::Calm, t, a);
            }
            return;
        }
        c.push_clip(&geo);
        match self.view {
            View::Listening => self.draw_listening(c, x0, s, a),
            View::Working => self.draw_working(c, x0, s, a),
            View::Reply => self.draw_reply(c, x0, s, a),
            View::Console => self.draw_console(c, x0, s, a),
            View::Menu => self.draw_menu(c, x0, s, a),
            View::Settings => self.draw_settings(c, x0, s, a),
            View::Confirm => self.draw_confirm(c, x0, s, a),
            View::Sleep => {}
        }
        c.pop_clip();
    }

    fn draw_listening(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let t = self.t0.elapsed().as_secs_f32();
        self.level_smooth += (self.level - self.level_smooth) * 0.35;
        let lv = self.level_smooth.clamp(0.0, 1.0);
        draw_orb(c, x0 + 32.0, s.d / 2.0, 12.5 * (1.0 + lv * 0.22), Mood::Listening, t, a);
        let label = if self.rec_tap { "Listening — pause to finish" } else { "Listening" };
        c.text(label, RectF::new(x0 + 56.0, s.d / 2.0 - 11.0, s.w - 140.0, 22.0), Font::Text, 14.0, 600, Align::Left, with_alpha(INK, a));
        // 7 bars
        let bx = x0 + s.w - 30.0 - 6.0 * 7.5;
        for (i, p) in BAR_PATTERN.iter().enumerate() {
            let wob = 0.55 + 0.45 * ((t * 9.0 + i as f32 * 1.3).sin() * 0.5 + 0.5);
            let h = 4.0 + 24.0 * p * (0.12 + lv * 0.88) * wob;
            let x = bx + i as f32 * 7.5;
            let col = if lv > 0.08 { EMBER } else { INK_DIM };
            c.fill_rect(RectF::new(x, s.d / 2.0 - h / 2.0, 3.6, h), 1.8, with_alpha(col, a));
        }
    }

    fn draw_working(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let t = self.t0.elapsed().as_secs_f32();
        draw_orb(c, x0 + 32.0, 28.0, 12.5, Mood::Thinking, t, a);
        let pulse = 0.62 + 0.38 * (t * 3.2).sin().abs();
        c.text(&self.status, RectF::new(x0 + 56.0, 17.0, s.w - 120.0, 22.0), Font::Text, 14.0, 600, Align::Left, with_alpha(INK, a * pulse));
        for i in 0..3 {
            let ph = ((t * 4.0 - i as f32 * 0.6).sin() * 0.5 + 0.5).powf(2.0);
            c.fill_circle(x0 + s.w - 48.0 + i as f32 * 9.0, 28.0, 2.6, with_alpha(EMBER, a * (0.25 + 0.75 * ph)));
        }
        if !self.heard.is_empty() {
            let q = format!("“{}”", self.heard);
            c.text(&q, RectF::new(x0 + 56.0, 44.0, s.w - 80.0, 22.0), Font::Text, 12.5, 400, Align::Left, with_alpha(INK_DIM, a));
        }
    }

    fn draw_reply(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let t = self.t0.elapsed().as_secs_f32();
        draw_orb(c, x0 + 30.0, 30.0, 12.0, if self.reply_error { Mood::Sad } else { Mood::Happy }, t, a);
        let col = if self.reply_error { rgba(0xFFD9D2, 1.0) } else { INK };
        c.text_wrapped(&self.reply, RectF::new(x0 + 52.0, 17.0, s.w - 52.0 - 26.0, s.d - 34.0), Font::Text, 14.0, 400, Align::Left, with_alpha(col, a));
        self.regions.push((RectF::new(x0, 0.0, s.w, s.d), Act::DismissReply));
    }

    fn draw_console(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let t = self.t0.elapsed().as_secs_f32();
        draw_orb(c, x0 + 30.0, s.d / 2.0, 11.0, Mood::Calm, t, a);
        let field = RectF::new(x0 + 54.0, s.d / 2.0 - 14.0, s.w - 54.0 - 60.0, 28.0);
        let caret = self.caret_on;
        c.push_clip_rect(field);
        self.console.draw(
            c,
            field,
            15.0,
            with_alpha(INK, a),
            "Ask anything — “move my PDFs from Downloads to Documents”",
            with_alpha(INK_FAINT, a),
            true,
            caret,
            with_alpha(EMBER, a),
        );
        c.pop_clip_rect();
        self.regions.push((field, Act::ConsoleField));
        let mic = RectF::new(x0 + s.w - 46.0, s.d / 2.0 - 15.0, 30.0, 30.0);
        let hov = self.hovered == Some(Act::Talk);
        c.fill_rect(mic, 15.0, with_alpha(if hov { CARD_HOVER } else { CARD }, a));
        c.icon(IC_MIC, mic.cx(), mic.cy(), 13.0, with_alpha(INK, a));
        self.regions.push((mic, Act::Talk));
    }

    fn button(&mut self, c: &mut Canvas, r: RectF, act: Act, a: f32, bg: Color, bg_hover: Color) {
        let hov = self.hovered == Some(act);
        c.fill_rect(r, r.h.min(r.w) / 2.0, with_alpha(if hov { bg_hover } else { bg }, a));
        self.regions.push((r, act));
    }

    fn draw_menu(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let pad = 18.0;
        let inner_w = s.w - 2.0 * pad;
        // --- now playing
        let card = RectF::new(x0 + pad, 18.0, inner_w, 86.0);
        c.fill_rect(card, 20.0, with_alpha(CARD, a));
        let art = RectF::new(card.x + 12.0, card.y + 12.0, 62.0, 62.0);
        c.hgradient_rect(art, 14.0, &[(0.0, with_alpha(rgba(0xF2C94C, 1.0), a)), (1.0, with_alpha(rgba(0xE0762A, 1.0), a))]);
        c.icon(IC_MUSIC, art.cx(), art.cy(), 20.0, with_alpha(EMBER_INK, a * 0.85));
        let tx = art.x + art.w + 14.0;
        let tw = card.x + card.w - tx - 132.0;
        let (title, artist) = if self.media.title.is_empty() {
            ("Nothing playing".to_string(), "Play something in Spotify, YouTube…".to_string())
        } else {
            (self.media.title.clone(), if self.media.artist.is_empty() { self.media.app.clone() } else { self.media.artist.clone() })
        };
        c.text(&title, RectF::new(tx, card.y + 20.0, tw, 22.0), Font::Text, 14.5, 600, Align::Left, with_alpha(INK, a));
        c.text(&artist, RectF::new(tx, card.y + 43.0, tw, 20.0), Font::Text, 12.5, 400, Align::Left, with_alpha(INK_DIM, a));
        let by = card.cy();
        let bx = card.x + card.w - 122.0;
        let prev = RectF::new(bx, by - 17.0, 34.0, 34.0);
        let play = RectF::new(bx + 40.0, by - 20.0, 40.0, 40.0);
        let next = RectF::new(bx + 86.0, by - 17.0, 34.0, 34.0);
        self.button(c, prev, Act::MediaPrev, a, rgba(0, 0.0), CARD_HOVER);
        c.icon(IC_PREV, prev.cx(), prev.cy(), 13.0, with_alpha(INK, a));
        let hov = self.hovered == Some(Act::MediaToggle);
        c.fill_circle(play.cx(), play.cy(), 20.0, with_alpha(if hov { rgba(0xFFFFFF, 1.0) } else { rgba(0xF4F3EE, 1.0) }, a));
        self.regions.push((play, Act::MediaToggle));
        c.icon(if self.media.playing { IC_PAUSE } else { IC_PLAY }, play.cx() + if self.media.playing { 0.0 } else { 1.0 }, play.cy(), 14.0, with_alpha(rgba(0x111111, 1.0), a));
        self.button(c, next, Act::MediaNext, a, rgba(0, 0.0), CARD_HOVER);
        c.icon(IC_NEXT, next.cx(), next.cy(), 13.0, with_alpha(INK, a));

        // --- volume
        let vy = card.y + card.h + 10.0;
        let row = RectF::new(x0 + pad, vy, inner_w, 42.0);
        c.fill_rect(row, 21.0, with_alpha(CARD, a));
        let mute = RectF::new(row.x + 5.0, row.y + 5.0, 32.0, 32.0);
        self.button(c, mute, Act::VolMute, a, rgba(0, 0.0), CARD_HOVER);
        c.icon(if self.muted { IC_MUTE } else { IC_VOL }, mute.cx(), mute.cy(), 13.0, with_alpha(if self.muted { INK_FAINT } else { INK }, a));
        let track = RectF::new(row.x + 48.0, row.cy() - 3.0, row.w - 48.0 - 58.0, 6.0);
        c.fill_rect(track, 3.0, with_alpha(rgba(0xFFFFFF, 0.12), a));
        let v = if self.muted { 0.0 } else { self.volume };
        c.hgradient_rect(RectF::new(track.x, track.y, (track.w * v).max(6.0), track.h), 3.0, &[(0.0, with_alpha(EMBER, a)), (1.0, with_alpha(EMBER_HOT, a))]);
        c.fill_circle(track.x + track.w * v, track.cy(), 7.5, with_alpha(INK, a));
        self.regions.push((RectF::new(track.x - 8.0, row.y, track.w + 16.0, row.h), Act::VolTrack));
        c.text(&format!("{}%", (v * 100.0).round()), RectF::new(row.x + row.w - 52.0, row.y, 40.0, row.h), Font::Text, 12.5, 600, Align::Right, with_alpha(INK_DIM, a));

        // --- tiles
        let ty = row.y + row.h + 10.0;
        let gap = 10.0;
        let tw = (inner_w - gap * 3.0) / 4.0;
        let tiles = [
            (IC_KEYBOARD, "Type", Act::OpenConsole),
            (IC_MIC, "Talk", Act::Talk),
            (IC_SETTINGS, "Settings", Act::OpenSettings),
            (IC_POWER, "Quit", Act::Quit),
        ];
        for (i, (ic, label, act)) in tiles.iter().enumerate() {
            let r = RectF::new(x0 + pad + i as f32 * (tw + gap), ty, tw, 62.0);
            let hov = self.hovered == Some(*act);
            c.fill_rect(r, 18.0, with_alpha(if hov { CARD_HOVER } else { CARD }, a));
            c.icon(*ic, r.cx(), r.y + 22.0, 14.0, with_alpha(if *act == Act::Quit { INK_DIM } else { INK }, a));
            c.text(label, RectF::new(r.x, r.y + 36.0, r.w, 18.0), Font::Text, 12.0, 500, Align::Center, with_alpha(INK_DIM, a));
            self.regions.push((r, *act));
        }

        // --- status line
        let sy = ty + 62.0 + 8.0;
        let cfg = config::get();
        let mut x = x0 + pad + 6.0;
        for (name, ok) in [("Gemini", !cfg.gemini_key.is_empty()), ("Groq", !cfg.groq_key.is_empty()), ("OpenRouter", !cfg.openrouter_key.is_empty())] {
            c.fill_circle(x + 3.0, sy + 13.0, 3.0, with_alpha(if ok { GREEN } else { INK_FAINT }, a));
            let wdt = c.g.measure(name, Font::Text, 12.0, 500, 200.0).0;
            c.text(name, RectF::new(x + 11.0, sy + 3.0, wdt + 4.0, 20.0), Font::Text, 12.0, 500, Align::Left, with_alpha(if ok { INK_DIM } else { INK_FAINT }, a));
            x += wdt + 26.0;
        }
        let (hk_talk, hk_type) = hotkeys();
        c.text(&format!("{hk_talk} talk · {hk_type} type"), RectF::new(x0 + s.w - pad - 250.0, sy + 3.0, 244.0, 20.0), Font::Text, 11.5, 400, Align::Right, with_alpha(INK_FAINT, a));
    }

    fn draw_settings(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let pad = 24.0;
        let iw = s.w - 2.0 * pad;
        let cfg = config::get();
        c.text("Settings", RectF::new(x0 + pad, 18.0, 200.0, 30.0), Font::Display, 20.0, 600, Align::Left, with_alpha(INK, a));
        let close = RectF::new(x0 + s.w - pad - 30.0, 18.0, 30.0, 30.0);
        self.button(c, close, Act::Close, a, rgba(0, 0.0), CARD_HOVER);
        c.icon(IC_CLOSE, close.cx(), close.cy(), 11.0, with_alpha(INK_DIM, a));
        c.text(
            "Connect any one free AI provider. Keys are encrypted on this PC and only sent to that provider.",
            RectF::new(x0 + pad, 50.0, iw, 20.0),
            Font::Text,
            12.5,
            400,
            Align::Left,
            with_alpha(INK_DIM, a),
        );

        // --- OpenRouter row
        let mut y = 82.0;
        let row = RectF::new(x0 + pad, y, iw, 58.0);
        c.fill_rect(row, 18.0, with_alpha(CARD, a));
        c.text("OpenRouter", RectF::new(row.x + 16.0, row.y + 9.0, 200.0, 22.0), Font::Text, 14.0, 600, Align::Left, with_alpha(INK, a));
        c.text("1-click sign-in · free models · text only", RectF::new(row.x + 16.0, row.y + 30.0, 260.0, 18.0), Font::Text, 12.0, 400, Align::Left, with_alpha(INK_DIM, a));
        if cfg.openrouter_key.is_empty() {
            let b = RectF::new(row.x + row.w - 12.0 - 118.0, row.cy() - 17.0, 118.0, 34.0);
            self.button(c, b, Act::ConnectOpenRouter, a, EMBER, rgba(0xFFD86A, 1.0));
            c.text("Connect", b, Font::Text, 13.0, 600, Align::Center, with_alpha(EMBER_INK, a));
        } else {
            c.icon(IC_CHECK, row.x + row.w - 150.0, row.cy(), 11.0, with_alpha(GREEN, a));
            c.text("Connected", RectF::new(row.x + row.w - 138.0, row.cy() - 10.0, 80.0, 20.0), Font::Text, 12.5, 600, Align::Left, with_alpha(GREEN, a));
            let b = RectF::new(row.x + row.w - 12.0 - 44.0, row.cy() - 15.0, 44.0, 30.0);
            self.button(c, b, Act::DisconnectOpenRouter, a, rgba(0, 0.0), CARD_HOVER);
            c.icon(IC_DELETE, b.cx(), b.cy(), 11.0, with_alpha(INK_DIM, a));
        }
        y += 58.0 + 10.0;

        // --- key rows
        let rows = [
            ("Gemini", "Most generous · hears & sees", &cfg.gemini_key, "Paste Gemini key (AIza…)"),
            ("Groq", "Fastest · free Whisper voice", &cfg.groq_key, "Paste Groq key (gsk_…)"),
        ];
        for (i, (name, sub, saved, ph)) in rows.iter().enumerate() {
            let row = RectF::new(x0 + pad, y, iw, 58.0);
            c.fill_rect(row, 18.0, with_alpha(CARD, a));
            c.text(name, RectF::new(row.x + 16.0, row.y + 9.0, 150.0, 22.0), Font::Text, 14.0, 600, Align::Left, with_alpha(INK, a));
            c.text(sub, RectF::new(row.x + 16.0, row.y + 30.0, 170.0, 18.0), Font::Text, 12.0, 400, Align::Left, with_alpha(INK_DIM, a));
            let fx = row.x + 196.0;
            let fw = row.w - 196.0 - 12.0 - 104.0 - if saved.is_empty() { 0.0 } else { 38.0 };
            let field = RectF::new(fx, row.cy() - 16.0, fw, 32.0);
            let focused = self.focus_field == Some(i);
            c.fill_rect(field, 10.0, with_alpha(rgba(0x000000, 0.55), a));
            c.stroke_rect(field, 10.0, with_alpha(if focused { EMBER } else { LINE }, a), 1.0);
            let placeholder = if saved.is_empty() {
                ph.to_string()
            } else {
                let tail: String = saved.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                format!("Saved ••••{tail} — paste to replace")
            };
            let inner = field.inset(10.0, 0.0);
            let caret = self.caret_on;
            c.push_clip_rect(inner);
            self.fields[i].draw(c, inner, 13.0, with_alpha(INK, a), &placeholder, with_alpha(INK_FAINT, a), focused, caret, with_alpha(EMBER, a));
            c.pop_clip_rect();
            self.regions.push((field, Act::Field(i)));
            let get = RectF::new(row.x + row.w - 12.0 - 100.0, row.cy() - 16.0, 100.0, 32.0);
            if !saved.is_empty() {
                let del = RectF::new(get.x - 40.0, row.cy() - 15.0, 34.0, 30.0);
                self.button(c, del, Act::ClearKey(i), a, rgba(0, 0.0), CARD_HOVER);
                c.icon(IC_DELETE, del.cx(), del.cy(), 11.0, with_alpha(INK_DIM, a));
            }
            self.button(c, get, Act::GetKey(i), a, CARD, CARD_HOVER);
            c.text("Get free key", RectF::new(get.x + 10.0, get.y, get.w - 30.0, get.h), Font::Text, 12.0, 600, Align::Left, with_alpha(INK, a));
            c.icon(IC_LINK, get.x + get.w - 15.0, get.cy(), 10.0, with_alpha(INK_DIM, a));
            y += 58.0 + 10.0;
        }

        // --- toggles
        y += 6.0;
        let toggles = [
            ("Speak replies aloud", "Uses Windows' built-in voices", cfg.voice_replies),
            ("Hide during fullscreen apps & games", "Never covers games or presentations", cfg.hide_in_fullscreen),
            ("Let it click & type in other apps", "Via Windows UI Automation — admin windows stay off-limits", cfg.allow_ui_control),
            ("Start with Windows", "Idles at ~0% CPU in the background", self.run_at_startup),
        ];
        for (i, (label, sub, on)) in toggles.iter().enumerate() {
            let r = RectF::new(x0 + pad, y, iw, 44.0);
            let hov = self.hovered == Some(Act::Toggle(i));
            if hov {
                c.fill_rect(r, 14.0, with_alpha(rgba(0xFFFFFF, 0.04), a));
            }
            c.text(label, RectF::new(r.x + 8.0, r.y + 3.0, r.w - 80.0, 20.0), Font::Text, 13.5, 500, Align::Left, with_alpha(INK, a));
            c.text(sub, RectF::new(r.x + 8.0, r.y + 22.0, r.w - 80.0, 18.0), Font::Text, 11.5, 400, Align::Left, with_alpha(INK_FAINT, a));
            draw_switch(c, r.x + r.w - 52.0, r.cy(), *on, a);
            self.regions.push((r, Act::Toggle(i)));
            y += 46.0;
        }

        // --- footer
        let fy = s.d - 58.0;
        c.line(x0 + pad, fy - 8.0, x0 + s.w - pad, fy - 8.0, with_alpha(LINE, a), 1.0);
        c.text(&self.toast.clone(), RectF::new(x0 + pad, fy, iw - 130.0, 36.0), Font::Text, 12.5, 500, Align::Left, with_alpha(EMBER, a));
        let save = RectF::new(x0 + s.w - pad - 110.0, fy, 110.0, 36.0);
        self.button(c, save, Act::Save, a, INK, rgba(0xFFFFFF, 1.0));
        c.text("Save", save, Font::Text, 13.0, 600, Align::Center, with_alpha(rgba(0x111111, 1.0), a));
    }

    fn draw_confirm(&mut self, c: &mut Canvas, x0: f32, s: Shape, a: f32) {
        let t = self.t0.elapsed().as_secs_f32();
        draw_orb(c, x0 + 32.0, 32.0, 13.0, Mood::Alert, t, a);
        c.text("Allow this?", RectF::new(x0 + 58.0, 19.0, s.w - 80.0, 24.0), Font::Text, 15.0, 600, Align::Left, with_alpha(INK, a));
        let p = self.confirm.as_ref().map(|c| c.0.clone()).unwrap_or_default();
        let (_, h) = c.g.measure(&p, Font::Text, 13.5, 400, s.w - 82.0);
        c.text_wrapped(&p, RectF::new(x0 + 58.0, 46.0, s.w - 82.0, h + 2.0), Font::Text, 13.5, 400, Align::Left, with_alpha(INK_DIM, a));
        let allow = RectF::new(x0 + s.w - 22.0 - 108.0, s.d - 22.0 - 36.0, 108.0, 36.0);
        let deny = RectF::new(allow.x - 10.0 - 96.0, allow.y, 96.0, 36.0);
        self.button(c, deny, Act::Deny, a, CARD, CARD_HOVER);
        c.text("Deny  Esc", deny, Font::Text, 13.0, 600, Align::Center, with_alpha(INK, a));
        self.button(c, allow, Act::Allow, a, EMBER, rgba(0xFFD86A, 1.0));
        c.text("Allow  ↵", allow, Font::Text, 13.0, 600, Align::Center, with_alpha(EMBER_INK, a));
    }
}

fn edit_key(tb: &mut TextBox, vk: VIRTUAL_KEY, ctrl: bool) {
    match vk {
        VK_LEFT => tb.key(Key::Left),
        VK_RIGHT => tb.key(Key::Right),
        VK_HOME => tb.key(Key::Home),
        VK_END => tb.key(Key::End),
        VK_DELETE => tb.key(Key::Delete),
        VK_BACK if ctrl => tb.key(Key::WordBackspace),
        VK_BACK => tb.key(Key::Backspace),
        VK_A if ctrl => tb.key(Key::SelectAll),
        VK_V if ctrl => {
            if let Some(t) = tools::system::clipboard_get() {
                tb.insert(t.trim());
            }
        }
        _ => {}
    }
}

fn draw_switch(c: &mut Canvas, x: f32, cy: f32, on: bool, a: f32) {
    let r = RectF::new(x, cy - 11.0, 40.0, 22.0);
    if on {
        c.fill_rect(r, 11.0, with_alpha(EMBER, a));
        c.fill_circle(r.x + r.w - 11.0, cy, 7.5, with_alpha(EMBER_INK, a));
    } else {
        c.stroke_rect(r.inset(0.5, 0.5), 10.5, with_alpha(rgba(0xFFFFFF, 0.35), a), 1.0);
        c.fill_circle(r.x + 11.0, cy, 6.0, with_alpha(INK_DIM, a));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mood {
    Calm,
    Listening,
    Thinking,
    Happy,
    Sad,
    Alert,
}

/// The mascot: a small glowing ember with eyes.
fn draw_orb(c: &mut Canvas, cx: f32, cy: f32, r: f32, mood: Mood, t: f32, a: f32) {
    c.radial(cx, cy, r * 2.1, &[(0.0, with_alpha(rgba(0xF2C94C, 0.40), a)), (0.5, with_alpha(rgba(0xFF9E45, 0.12), a)), (1.0, with_alpha(rgba(0xFF9E45, 0.0), a))]);
    c.radial_offset(
        cx,
        cy,
        r,
        -r * 0.3,
        -r * 0.35,
        &[(0.0, with_alpha(rgba(0xFFF6D6, 1.0), a)), (0.55, with_alpha(rgba(0xF6CF55, 1.0), a)), (1.0, with_alpha(rgba(0xEE9A3A, 1.0), a))],
    );
    let eye = with_alpha(EMBER_INK, a);
    let (ex, ey) = (r * 0.34, cy + r * 0.02);
    let look = match mood {
        Mood::Thinking => (t * 2.2).sin() * r * 0.12,
        _ => 0.0,
    };
    let lift = if mood == Mood::Listening { -r * 0.08 } else { 0.0 };
    // blink roughly every 3.7s while animating
    let blink = ((t % 3.7) < 0.12) && matches!(mood, Mood::Listening | Mood::Thinking);
    match mood {
        Mood::Happy => {
            for sx in [-1.0f32, 1.0] {
                let x = cx + sx * ex;
                c.line(x - r * 0.13, ey + r * 0.04, x, ey - r * 0.1, eye, r * 0.11);
                c.line(x, ey - r * 0.1, x + r * 0.13, ey + r * 0.04, eye, r * 0.11);
            }
        }
        Mood::Sad => {
            for sx in [-1.0f32, 1.0] {
                let x = cx + sx * ex;
                c.fill_ellipse(x, ey + r * 0.04, r * 0.09, r * 0.11, eye);
                c.line(x - r * 0.14, ey - r * 0.2 - sx * r * 0.05, x + r * 0.14, ey - r * 0.2 + sx * r * 0.05, eye, r * 0.08);
            }
        }
        Mood::Alert => {
            for sx in [-1.0f32, 1.0] {
                c.fill_ellipse(cx + sx * ex, ey, r * 0.12, r * 0.18, eye);
            }
        }
        _ => {
            let ry = if blink { r * 0.025 } else { r * 0.16 };
            for sx in [-1.0f32, 1.0] {
                c.fill_ellipse(cx + sx * ex + look, ey + lift, r * 0.1, ry, eye);
            }
        }
    }
}

/// Tapping Alt alone makes the focused app highlight its menu bar; a dummy
/// key press in between cancels that, the same trick PowerToys uses.
fn suppress_alt_menu() {
    unsafe {
        let mk = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0xE8), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        let inputs = [mk(KEYBD_EVENT_FLAGS(0)), mk(KEYEVENTF_KEYUP)];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn lparam_xy(l: LPARAM) -> (i32, i32) {
    let x = (l.0 & 0xffff) as i16 as i32;
    let y = ((l.0 >> 16) & 0xffff) as i16 as i32;
    (x, y)
}

pub unsafe extern "system" fn on_win_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    unsafe {
        if !ISLAND.is_null() {
            get().on_foreground_change();
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if ISLAND.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let me = get();
        match msg {
            WM_UI_EVENT => {
                me.on_events();
                LRESULT(0)
            }
            WM_HOTKEY => {
                match wp.0 as i32 {
                    HK_TALK => me.talk(),
                    HK_CONSOLE => {
                        if me.view == View::Console {
                            me.set_view(View::Sleep)
                        } else {
                            me.open_console()
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIMER => {
                me.on_timer(wp.0);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                me.on_mouse_move(x as f32 / me.scale, y as f32 / me.scale);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                me.on_mouse_leave();
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_xy(lp);
                me.on_click(x as f32 / me.scale, y as f32 / me.scale);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if me.mouse_down_track {
                    me.mouse_down_track = false;
                    let _ = ReleaseCapture();
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as i16;
                if delta < 0 && me.view == View::Sleep {
                    me.set_view(View::Menu);
                } else if delta > 0 && me.view == View::Menu {
                    me.set_view(View::Sleep);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                tray::show_menu(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                me.on_key(VIRTUAL_KEY(wp.0 as u16));
                if msg == WM_SYSKEYDOWN {
                    return DefWindowProcW(hwnd, msg, wp, lp);
                }
                LRESULT(0)
            }
            WM_CHAR => {
                if let Some(ch) = char::from_u32(wp.0 as u32) {
                    me.on_char(ch);
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                let inactive = (wp.0 & 0xffff) as u32 == WA_INACTIVE;
                crate::util::log(&format!("WM_ACTIVATE inactive={inactive} view={:?} other={:?}", me.view, lp));
                if inactive && me.view == View::Console {
                    me.activatable = false;
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) | WS_EX_NOACTIVATE.0 as isize;
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
                    me.set_view(View::Sleep);
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => {
                if me.activatable {
                    LRESULT(MA_ACTIVATE as isize)
                } else {
                    LRESULT(MA_NOACTIVATE as isize)
                }
            }
            WM_CLIPBOARDUPDATE => {
                if me.view == View::Settings {
                    me.sniff_clipboard();
                    me.present();
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
                me.update_monitor();
                me.present();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            m if m == tray::WM_TRAY => {
                tray::on_message(hwnd, lp);
                LRESULT(0)
            }
            m if m == tray::taskbar_created() => {
                tray::add(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                match tray::Cmd::from(wp.0 & 0xffff) {
                    Some(tray::Cmd::Talk) => me.talk(),
                    Some(tray::Cmd::Type) => me.open_console(),
                    Some(tray::Cmd::Menu) => me.set_view(View::Menu),
                    Some(tray::Cmd::Settings) => me.open_settings(""),
                    Some(tray::Cmd::Quit) => {
                        let _ = DestroyWindow(hwnd);
                    }
                    None => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                tray::remove(hwnd);
                tts::stop();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

impl Island {
    pub fn on_tray_click(&mut self) {
        if self.view == View::Menu {
            self.set_view(View::Sleep);
        } else {
            self.set_view(View::Menu);
        }
    }

    pub fn first_run(&mut self) {
        if !config::get().has_any_key() {
            self.open_settings("Welcome! Connect any free provider to get started.");
        }
    }
}
