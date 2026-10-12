//! Folders and files. Works on the file system directly (more reliable than
//! driving Explorer's UI), with guard rails: system folders are off-limits,
//! nothing is overwritten, deletes go to the Recycle Bin after an "Allow".

use crate::util::{fuzzy_score, wide};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::*;
use windows::core::GUID;

fn known(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

fn home() -> PathBuf {
    known(&FOLDERID_Profile).unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())))
}

fn known_by_name(name: &str) -> Option<PathBuf> {
    let n = name.trim().to_lowercase();
    let n = n.trim_start_matches("my ").trim_end_matches(" folder").trim();
    let id = match n {
        "desktop" => &FOLDERID_Desktop,
        "downloads" | "download" => &FOLDERID_Downloads,
        "documents" | "document" | "docs" => &FOLDERID_Documents,
        "pictures" | "photos" | "images" => &FOLDERID_Pictures,
        "music" => &FOLDERID_Music,
        "videos" | "video" => &FOLDERID_Videos,
        "screenshots" => &FOLDERID_Screenshots,
        "home" | "user" | "profile" | "~" => return Some(home()),
        "appdata" => &FOLDERID_RoamingAppData,
        "temp" => return Some(std::env::temp_dir()),
        _ => return None,
    };
    known(id)
}

/// Resolves things like "downloads", "downloads/invoices", "~/x", "%TEMP%",
/// or absolute paths.
pub fn resolve(input: &str) -> PathBuf {
    let s = input.trim().trim_matches('"');
    if s.is_empty() {
        return home();
    }
    let s = if s.contains('%') {
        let mut out = s.to_string();
        for (k, v) in std::env::vars() {
            out = out.replace(&format!("%{k}%"), &v).replace(&format!("%{}%", k.to_lowercase()), &v);
        }
        out
    } else {
        s.to_string()
    };
    let norm = s.replace('/', "\\");
    if let Some(rest) = norm.strip_prefix("~\\").or_else(|| norm.strip_prefix('~')) {
        return home().join(rest.trim_start_matches('\\'));
    }
    let p = PathBuf::from(&norm);
    if p.is_absolute() {
        return p;
    }
    let (first, rest) = match norm.split_once('\\') {
        Some((a, b)) => (a.to_string(), Some(b.to_string())),
        None => (norm.clone(), None),
    };
    if let Some(base) = known_by_name(&first) {
        return match rest {
            Some(r) => base.join(r),
            None => base,
        };
    }
    home().join(norm)
}

fn protected(p: &Path) -> bool {
    let s = p.to_string_lossy().to_lowercase();
    let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()).to_lowercase();
    s.starts_with(&win)
        || s.contains("\\program files")
        || s.contains("\\programdata")
        || s.ends_with(":\\")
        || s.ends_with(":")
        || p == home()
}

pub fn open(target: &str) -> Result<String, String> {
    let t = target.trim();
    let tl = t.to_lowercase();
    let special = match tl.as_str() {
        "recycle bin" | "trash" => Some("shell:RecycleBinFolder"),
        "this pc" | "my computer" | "computer" => Some("shell:MyComputerFolder"),
        "control panel" => Some("control"),
        "settings" => Some("ms-settings:"),
        "startup" => Some("shell:startup"),
        _ => None,
    };
    if let Some(s) = special {
        super::shell_open(s);
        return Ok(format!("Opened {t}."));
    }
    if tl.starts_with("http://") || tl.starts_with("https://") || tl.starts_with("www.") {
        let url = if tl.starts_with("www.") { format!("https://{t}") } else { t.to_string() };
        super::shell_open(&url);
        return Ok(format!("Opened {url}."));
    }
    let p = resolve(t);
    if p.exists() {
        if super::shell_open(&p.to_string_lossy()) {
            return Ok(format!("Opened {}.", p.display()));
        }
        return Err(format!("couldn't open {}", p.display()));
    }
    // Maybe they meant an app.
    super::apps::open(t).map_err(|_| format!("{} doesn't exist", p.display()))
}

fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

pub fn list(folder: &str, pattern: &str) -> Result<String, String> {
    let dir = resolve(folder);
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    let mut entries: Vec<(SystemTime, String)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !pattern.is_empty() && !wildcard(pattern, &name) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let modified = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let label = if md.is_dir() { format!("[folder] {name}") } else { format!("{name} ({})", human_size(md.len())) };
        entries.push((modified, label));
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let total = entries.len();
    let shown: Vec<String> = entries.into_iter().take(60).map(|(_, l)| l).collect();
    let more = if total > 60 { format!("\n…and {} more", total - 60) } else { String::new() };
    Ok(format!("{} — {} item(s):\n{}{}", dir.display(), total, shown.join("\n"), more))
}

pub fn find(name: &str, folder: &str) -> Result<String, String> {
    let root = if folder.is_empty() { home() } else { resolve(folder) };
    let mut hits = Vec::new();
    let pat = if name.contains('*') || name.contains('?') { name.to_string() } else { String::new() };
    let mut stack = vec![(root.clone(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > 4000 || hits.len() >= 30 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with('.') || fname == "AppData" || fname == "node_modules" {
                continue;
            }
            let matched = if pat.is_empty() { fuzzy_score(&fname, name) >= 400 } else { wildcard(&pat, &fname) };
            if matched {
                hits.push(e.path().display().to_string());
            }
            if depth < 4 && e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    if hits.is_empty() {
        Ok(format!("No files matching “{name}” under {}.", root.display()))
    } else {
        Ok(format!("Found {}:\n{}", hits.len(), hits.join("\n")))
    }
}

/// Expands paths with wildcards into existing files.
fn expand(paths: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        let full = resolve(p);
        let fname = full.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        if fname.contains('*') || fname.contains('?') {
            if let Some(parent) = full.parent() {
                if let Ok(rd) = std::fs::read_dir(parent) {
                    for e in rd.flatten() {
                        if wildcard(&fname, &e.file_name().to_string_lossy()) {
                            out.push(e.path());
                        }
                    }
                }
            }
        } else if full.exists() {
            out.push(full);
        }
    }
    out
}

fn unique_dest(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 1..1000 {
        let c = dir.join(format!("{stem} ({i}){ext}"));
        if !c.exists() {
            return c;
        }
    }
    candidate
}

fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)?.flatten() {
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn recycle(paths: &[PathBuf]) -> Result<(), String> {
    // Double-NUL-terminated list for SHFileOperation.
    let mut buf: Vec<u16> = Vec::new();
    for p in paths {
        let w = wide(&p.to_string_lossy());
        buf.extend_from_slice(&w);
    }
    buf.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(buf.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(format!("the Recycle Bin operation failed (code {r})"));
    }
    Ok(())
}

pub fn op(args: &Value) -> Result<String, String> {
    let op = args.get("op").and_then(Value::as_str).unwrap_or("");
    let paths: Vec<String> = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .or_else(|| args.get("paths").and_then(Value::as_str).map(|s| vec![s.to_string()]))
        .unwrap_or_default();
    let to = args.get("to").and_then(Value::as_str).unwrap_or("").trim();

    if op == "mkdir" {
        let mut made = Vec::new();
        for p in &paths {
            let dir = resolve(p);
            if protected(&dir) {
                return Err(format!("{} is a protected location", dir.display()));
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            made.push(dir.display().to_string());
        }
        return Ok(format!("Created {}.", made.join(", ")));
    }

    let files = expand(&paths);
    if files.is_empty() {
        return Err("no files matched".into());
    }
    if let Some(p) = files.iter().find(|p| protected(p)) {
        return Err(format!("{} is in a protected system location; I won't touch it", p.display()));
    }
    let names = |fs: &[PathBuf]| {
        let mut n: Vec<String> = fs.iter().take(5).map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string()).collect();
        if fs.len() > 5 {
            n.push(format!("+{} more", fs.len() - 5));
        }
        n.join(", ")
    };

    match op {
        "delete" => {
            if !super::confirm(&format!("Move {} item(s) to the Recycle Bin: {}", files.len(), names(&files))) {
                return Ok("The user declined; nothing was deleted.".into());
            }
            recycle(&files)?;
            Ok(format!("Moved {} item(s) to the Recycle Bin (restorable).", files.len()))
        }
        "rename" => {
            if files.len() != 1 || to.is_empty() {
                return Err("rename needs exactly one file and a new name in 'to'".into());
            }
            let src = &files[0];
            let dest = src.with_file_name(to);
            if dest.exists() {
                return Err(format!("{} already exists", dest.display()));
            }
            std::fs::rename(src, &dest).map_err(|e| e.to_string())?;
            Ok(format!("Renamed to {}.", dest.display()))
        }
        "move" | "copy" => {
            if to.is_empty() {
                return Err("where to? give a destination folder in 'to'".into());
            }
            let dest_dir = resolve(to);
            if protected(&dest_dir) {
                return Err(format!("{} is a protected location", dest_dir.display()));
            }
            if files.len() > 25 && !super::confirm(&format!("{} {} items to {}?", if op == "move" { "Move" } else { "Copy" }, files.len(), dest_dir.display())) {
                return Ok("The user declined.".into());
            }
            std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
            let mut done = 0;
            let mut failed = Vec::new();
            for f in &files {
                let Some(name) = f.file_name() else { continue };
                if dest_dir.starts_with(f) {
                    continue; // never move a folder into itself
                }
                let dest = unique_dest(&dest_dir, name);
                let r = if op == "move" {
                    std::fs::rename(f, &dest).or_else(|_| copy_recursive(f, &dest).and_then(|_| if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) }))
                } else {
                    copy_recursive(f, &dest)
                };
                match r {
                    Ok(_) => done += 1,
                    Err(e) => failed.push(format!("{}: {e}", name.to_string_lossy())),
                }
            }
            let verb = if op == "move" { "Moved" } else { "Copied" };
            let mut msg = format!("{verb} {done} item(s) to {}.", dest_dir.display());
            if !failed.is_empty() {
                msg.push_str(&format!(" Failed: {}", failed.join("; ")));
            }
            Ok(msg)
        }
        _ => Err(format!("unknown op {op}")),
    }
}
