//! Acting inside other apps through UI Automation, the accessibility layer
//! screen readers use. Controls are found by their visible names and invoked
//! directly (no pixel guessing). Apps with poor accessibility expose little;
//! we say so rather than clicking blindly.

use super::ComGuard;
use crate::util::{fuzzy_score, truncate};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
use windows::core::{BSTR, Interface};

const MAX_SCAN: i32 = 600;

struct Found {
    el: IUIAutomationElement,
    name: String,
    kind: &'static str,
}

#[allow(non_upper_case_globals)]
fn kind_name(id: UIA_CONTROLTYPE_ID) -> &'static str {
    match id {
        UIA_ButtonControlTypeId => "button",
        UIA_SplitButtonControlTypeId => "button",
        UIA_MenuItemControlTypeId => "menu item",
        UIA_HyperlinkControlTypeId => "link",
        UIA_TabItemControlTypeId => "tab",
        UIA_ListItemControlTypeId => "list item",
        UIA_TreeItemControlTypeId => "tree item",
        UIA_CheckBoxControlTypeId => "checkbox",
        UIA_RadioButtonControlTypeId => "radio",
        UIA_ComboBoxControlTypeId => "dropdown",
        UIA_EditControlTypeId => "text field",
        UIA_DocumentControlTypeId => "document",
        _ => "control",
    }
}

fn target(window: &str) -> Result<HWND, String> {
    let h = super::wins::find(window).ok_or_else(|| format!("no window matching “{window}”"))?;
    if super::blocked_by_elevation(h) {
        return Err(super::ELEVATED_MSG.into());
    }
    Ok(h)
}

fn scan(hwnd: HWND, fields_only: bool) -> Result<Vec<Found>, String> {
    unsafe {
        let auto: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|e| e.message())?;
        let root = auto.ElementFromHandle(hwnd).map_err(|e| e.message())?;
        let types: &[UIA_CONTROLTYPE_ID] = if fields_only {
            &[UIA_EditControlTypeId, UIA_DocumentControlTypeId, UIA_ComboBoxControlTypeId]
        } else {
            &[
                UIA_ButtonControlTypeId,
                UIA_SplitButtonControlTypeId,
                UIA_MenuItemControlTypeId,
                UIA_HyperlinkControlTypeId,
                UIA_TabItemControlTypeId,
                UIA_ListItemControlTypeId,
                UIA_TreeItemControlTypeId,
                UIA_CheckBoxControlTypeId,
                UIA_RadioButtonControlTypeId,
                UIA_ComboBoxControlTypeId,
                UIA_EditControlTypeId,
            ]
        };
        let mut cond: Option<IUIAutomationCondition> = None;
        for t in types {
            let c = auto.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(t.0)).map_err(|e| e.message())?;
            cond = Some(match cond {
                None => c,
                Some(prev) => auto.CreateOrCondition(&prev, &c).map_err(|e| e.message())?.cast().map_err(|e| e.message())?,
            });
        }
        let all = root.FindAll(TreeScope_Descendants, &cond.unwrap()).map_err(|e| e.message())?;
        let n = all.Length().unwrap_or(0).min(MAX_SCAN);
        let mut out = Vec::new();
        for i in 0..n {
            let Ok(el) = all.GetElement(i) else { continue };
            if el.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or(false) {
                continue;
            }
            let name = el.CurrentName().map(|b| b.to_string()).unwrap_or_default();
            let kind = kind_name(el.CurrentControlType().unwrap_or_default());
            if name.trim().is_empty() && kind != "text field" && kind != "document" {
                continue;
            }
            out.push(Found { el, name: name.trim().to_string(), kind });
        }
        Ok(out)
    }
}

pub fn list(window: &str) -> Result<String, String> {
    let _com = ComGuard::mta();
    let h = target(window)?;
    let found = scan(h, false)?;
    if found.is_empty() {
        return Ok("This window exposes no named controls to accessibility tools (common in games and some custom apps).".into());
    }
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<String> = found
        .iter()
        .filter(|f| seen.insert((f.kind, f.name.clone())))
        .take(90)
        .map(|f| format!("{}: {}", f.kind, truncate(if f.name.is_empty() { "(unnamed)" } else { &f.name }, 60)))
        .collect();
    Ok(lines.join("\n"))
}

fn best(found: Vec<Found>, name: &str) -> Option<Found> {
    found.into_iter().map(|f| (fuzzy_score(&f.name, name), f)).filter(|(s, _)| *s > 0).max_by_key(|(s, _)| *s).map(|(_, f)| f)
}

unsafe fn activate(el: &IUIAutomationElement) -> bool {
    unsafe {
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
            if p.Invoke().is_ok() {
                return true;
            }
        }
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
            if p.Toggle().is_ok() {
                return true;
            }
        }
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) {
            if p.Select().is_ok() {
                return true;
            }
        }
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId) {
            if p.Expand().is_ok() {
                return true;
            }
        }
        // Fall back to a real click in the middle of the control.
        if let Ok(r) = el.CurrentBoundingRectangle() {
            if r.right > r.left && r.bottom > r.top {
                let _ = SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                let mk = |f| INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: f, time: 0, dwExtraInfo: 0 } } };
                let clicks = [mk(MOUSEEVENTF_LEFTDOWN), mk(MOUSEEVENTF_LEFTUP)];
                SendInput(&clicks, std::mem::size_of::<INPUT>() as i32);
                return true;
            }
        }
        false
    }
}

pub fn click(name: &str, window: &str) -> Result<String, String> {
    let _com = ComGuard::mta();
    let h = target(window)?;
    super::wins::force_foreground(h);
    let found = scan(h, false)?;
    let f = best(found, name).ok_or_else(|| format!("no control named “{name}” in that window; try ui_list to see what's there"))?;
    if unsafe { activate(&f.el) } {
        Ok(format!("Clicked {} “{}”.", f.kind, f.name))
    } else {
        Err(format!("found “{}” but it couldn't be clicked", f.name))
    }
}

pub fn type_into(text: &str, field: &str, window: &str) -> Result<String, String> {
    let _com = ComGuard::mta();
    let h = target(window)?;
    super::wins::force_foreground(h);
    std::thread::sleep(std::time::Duration::from_millis(120));
    if !field.is_empty() {
        let found = scan(h, true)?;
        let f = best(found, field).ok_or_else(|| format!("no text field named “{field}”"))?;
        unsafe {
            if let Ok(v) = f.el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                if v.SetValue(&BSTR::from(text)).is_ok() {
                    return Ok(format!("Typed into “{}”.", f.name));
                }
            }
            let _ = f.el.SetFocus();
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
    super::input::type_text(text);
    Ok("Typed it.".into())
}
