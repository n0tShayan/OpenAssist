//! App launching through the shell's AppsFolder: the same list as the Start
//! menu, covering both desktop and Store apps (e.g. Spotify from the Store).

use crate::util::fuzzy_score;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::*;

struct App {
    name: String,
    id: String,
}

static CACHE: Mutex<Option<(Instant, Vec<App>)>> = Mutex::new(None);

/// Shell folders want a single-threaded apartment; use a short-lived one so
/// callers can be on any thread.
fn enumerate() -> Vec<App> {
    std::thread::spawn(|| {
        let _com = super::ComGuard::sta();
        enumerate_sta()
    })
    .join()
    .unwrap_or_default()
}

fn enumerate_sta() -> Vec<App> {
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return out };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        loop {
            let mut batch = [None];
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = batch[0].take() else { break };
            let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok();
            let id = item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING).ok();
            if let (Some(n), Some(i)) = (name, id) {
                let app = App { name: n.to_string().unwrap_or_default(), id: i.to_string().unwrap_or_default() };
                CoTaskMemFree(Some(n.0 as _));
                CoTaskMemFree(Some(i.0 as _));
                if !app.name.is_empty() {
                    out.push(app);
                }
            }
        }
    }
    out
}

fn with_apps<R>(f: impl FnOnce(&[App]) -> R) -> R {
    let mut c = CACHE.lock().unwrap();
    let stale = c.as_ref().map(|(t, _)| t.elapsed() > Duration::from_secs(600)).unwrap_or(true);
    if stale {
        *c = Some((Instant::now(), enumerate()));
    }
    f(&c.as_ref().unwrap().1)
}

/// Drop the cached list (a few KB) when idle.
pub fn forget() {
    *CACHE.lock().unwrap() = None;
}

pub fn is_installed(name: &str) -> bool {
    with_apps(|apps| apps.iter().any(|a| fuzzy_score(&a.name, name) >= 600))
}

pub fn find(name: &str) -> Option<(String, String)> {
    let skip = ["uninstall", "readme", "help", "documentation", "release notes"];
    with_apps(|apps| {
        apps.iter()
            .filter(|a| !skip.iter().any(|s| a.name.to_lowercase().contains(s)))
            .map(|a| (fuzzy_score(&a.name, name), a))
            .filter(|(s, _)| *s > 0)
            .max_by_key(|(s, a)| (*s, -(a.name.len() as i32)))
            .map(|(_, a)| (a.name.clone(), a.id.clone()))
    })
}

pub fn open(name: &str) -> Result<String, String> {
    let name = name.trim().trim_end_matches(".exe");
    if name.is_empty() {
        return Err("which app?".into());
    }
    if let Some((display, id)) = find(name) {
        if super::shell_open(&format!("shell:AppsFolder\\{id}")) {
            return Ok(format!("Opened {display}."));
        }
    }
    // Last resort: let the shell resolve it (e.g. "notepad", "calc").
    if super::shell_open(name) {
        return Ok(format!("Opened {name}."));
    }
    Err(format!("couldn't find an app called “{name}”"))
}
