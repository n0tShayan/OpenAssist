//! Keyboard input via SendInput. Windows silently drops input aimed at
//! elevated windows, so callers check `blocked_by_elevation` first.

use windows::Win32::UI::Input::KeyboardAndMouse::*;

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let extended = matches!(
        vk,
        VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT | VK_INSERT | VK_DELETE | VK_LWIN | VK_RWIN | VK_RMENU | VK_RCONTROL
    );
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
}

fn send(inputs: &[INPUT]) -> u32 {
    unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) }
}

pub fn tap_vk(vk: VIRTUAL_KEY) {
    send(&[key(vk, false), key(vk, true)]);
}

pub fn press_combo(keys: &[VIRTUAL_KEY]) {
    let mut v: Vec<INPUT> = keys.iter().map(|k| key(*k, false)).collect();
    v.extend(keys.iter().rev().map(|k| key(*k, true)));
    send(&v);
}

/// Types arbitrary Unicode text.
pub fn type_text(text: &str) {
    let mut v = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        if unit == '\n' as u16 {
            v.push(key(VK_RETURN, false));
            v.push(key(VK_RETURN, true));
            continue;
        }
        for up in [false, true] {
            let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
            v.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            });
        }
    }
    for chunk in v.chunks(200) {
        send(chunk);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn parse_key(k: &str) -> Option<VIRTUAL_KEY> {
    let k = k.trim().to_lowercase();
    Some(match k.as_str() {
        "ctrl" | "control" => VK_CONTROL,
        "shift" => VK_SHIFT,
        "alt" => VK_MENU,
        "win" | "windows" | "super" | "meta" => VK_LWIN,
        "enter" | "return" => VK_RETURN,
        "esc" | "escape" => VK_ESCAPE,
        "tab" => VK_TAB,
        "space" => VK_SPACE,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "printscreen" => VK_SNAPSHOT,
        "plus" | "=" => VK_OEM_PLUS,
        "minus" | "-" => VK_OEM_MINUS,
        s if s.len() > 1 && s.starts_with('f') && s[1..].parse::<u16>().map(|n| (1..=24).contains(&n)).unwrap_or(false) => {
            VIRTUAL_KEY(VK_F1.0 + s[1..].parse::<u16>().unwrap() - 1)
        }
        s if s.len() == 1 => {
            let c = s.chars().next()?.to_ascii_uppercase();
            if c.is_ascii_alphanumeric() { VIRTUAL_KEY(c as u16) } else { return None }
        }
        _ => return None,
    })
}

pub fn press_combo_in(keys: &str, window: &str) -> Result<String, String> {
    let combo: Option<Vec<VIRTUAL_KEY>> = keys.split('+').map(parse_key).collect();
    let combo = combo.ok_or_else(|| format!("don't know the key combo “{keys}”"))?;
    let target = super::wins::find(window).ok_or("no target window")?;
    if super::blocked_by_elevation(target) {
        return Err(super::ELEVATED_MSG.into());
    }
    super::wins::force_foreground(target);
    std::thread::sleep(std::time::Duration::from_millis(120));
    press_combo(&combo);
    Ok(format!("Pressed {keys}."))
}
