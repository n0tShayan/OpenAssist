#![windows_subsystem = "windows"]
//! OpenAssist: a featherweight voice assistant that lives in a notch at the
//! top of the screen. Pure Win32 + Direct2D, no web view, no runtime.
//!
//! Idle cost: the UI thread sleeps in GetMessage, no timers run, the frame
//! buffer is released, and the working set is trimmed. Windows runs the
//! process in Efficiency mode (EcoQoS) until you talk to it.

mod agent;
mod audio;
mod config;
mod events;
mod gfx;
mod intents;
mod island;
mod llm;
mod oauth;
mod textbox;
mod tools;
mod tray;
mod tts;
mod util;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::SetWinEventHook;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// Gives idle memory back to Windows. Pages come back on demand.
pub fn trim_memory() {
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

/// EcoQoS: lets Windows run us on efficient cores at low clocks while idle.
pub fn efficiency_mode(on: bool) {
    unsafe {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
        };
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

/// Developer commands, e.g. `openassist --tool list_files "{\"folder\":\"downloads\"}"`.
fn cli(args: &[String]) -> bool {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    let Some(cmd) = args.get(1) else { return false };
    if !cmd.starts_with("--") {
        return false;
    }
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let _com = tools::ComGuard::mta();
    match cmd.as_str() {
        "--tool" => {
            let name = args.get(2).map(String::as_str).unwrap_or("");
            let json = args.get(3).map(String::as_str).unwrap_or("{}");
            let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
            println!("{}", tools::execute(name, &v));
        }
        "--intent" => {
            let text = args[2..].join(" ");
            println!("{:?}", intents::try_local(&text));
        }
        "--free-models" => println!("{:?}", llm::free_openrouter_model(None)),
        "--tts-test" => println!("{}", tts::synth_test(args.get(2).map(String::as_str).unwrap_or("Hello from OpenAssist."))),
        _ => println!("commands: --tool <name> <json>, --intent <text>, --free-models, --tts-test [text]"),
    }
    true
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if cli(&args) {
        return;
    }
    unsafe {
        // One instance only.
        let _mutex = CreateMutexW(None, true, w!("Local\\OpenAssist.Singleton"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let Ok(_hwnd) = island::create() else { return };

        // Out-of-context foreground hook: lets the island step aside for
        // fullscreen games/presentations. Costs nothing between switches.
        let _hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(island::on_win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );

        island::get().first_run();
        efficiency_mode(true);
        trim_memory();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
