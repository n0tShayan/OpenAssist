//! Master volume via the default playback device's endpoint volume.

use serde_json::Value;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

fn endpoint() -> Result<IAudioEndpointVolume, String> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.message())?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "no speakers found".to_string())?;
        dev.Activate(CLSCTX_ALL, None).map_err(|e| e.message())
    }
}

pub fn get() -> Result<(f32, bool), String> {
    let ep = endpoint()?;
    unsafe {
        let v = ep.GetMasterVolumeLevelScalar().map_err(|e| e.message())?;
        let m = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        Ok((v, m))
    }
}

pub fn set(v: f32) -> Result<(), String> {
    let ep = endpoint()?;
    unsafe { ep.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null()).map_err(|e| e.message()) }
}

pub fn set_mute(m: bool) -> Result<(), String> {
    let ep = endpoint()?;
    unsafe { ep.SetMute(m, std::ptr::null()).map_err(|e| e.message()) }
}

pub fn tool(args: &Value) -> Result<String, String> {
    if let Some(m) = args.get("mute").and_then(Value::as_bool) {
        set_mute(m)?;
        if args.get("level").is_none() && args.get("change").is_none() {
            return Ok(if m { "Muted.".into() } else { "Unmuted.".into() });
        }
    }
    let (cur, _) = get()?;
    let target = if let Some(l) = args.get("level").and_then(Value::as_f64) {
        l as f32 / 100.0
    } else if let Some(c) = args.get("change").and_then(Value::as_f64) {
        cur + c as f32 / 100.0
    } else {
        return Ok(format!("Volume is {}%.", (cur * 100.0).round()));
    };
    set(target)?;
    let _ = set_mute(false);
    Ok(format!("Volume set to {}%.", (target.clamp(0.0, 1.0) * 100.0).round()))
}
