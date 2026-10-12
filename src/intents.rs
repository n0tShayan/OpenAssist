//! Local fast path: common commands are understood without any AI call.
//! Instant, offline, and they don't spend the free quota.

use crate::tools::{self, files, media, system, volume};
use serde_json::json;

fn normalize(text: &str) -> String {
    let t = text.to_lowercase();
    let t: String = t.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '%' || c == '\'' { c } else { ' ' }).collect();
    let mut t = format!(" {} ", t.split_whitespace().collect::<Vec<_>>().join(" "));
    for filler in [" please ", " hey ", " can you ", " could you ", " would you ", " for me ", " now ", " the ", " my ", " a ", " just ", " okay ", " ok "] {
        while t.contains(filler) {
            t = t.replace(filler, " ");
        }
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn number(t: &str) -> Option<i64> {
    t.split_whitespace().find_map(|w| w.trim_end_matches('%').parse::<i64>().ok())
}

/// Returns a reply if the command was handled locally.
pub fn try_local(text: &str) -> Option<String> {
    let t = normalize(text);
    let has = |w: &str| t.split_whitespace().any(|x| x == w);
    let starts = |p: &str| t.starts_with(p);

    // --- media
    match t.as_str() {
        "next" | "next song" | "next track" | "skip" | "skip song" | "skip this" | "skip this song" | "skip track" | "play next song" => {
            return Some(media::control("next").map(|_| "Skipped.".into()).unwrap_or_else(|e| e));
        }
        "previous" | "previous song" | "previous track" | "last song" | "go back" | "play previous song" | "back" => {
            return Some(media::control("previous").map(|_| "Going back.".into()).unwrap_or_else(|e| e));
        }
        "pause" | "pause music" | "pause song" | "stop music" | "pause it" | "stop song" => {
            return Some(media::control("pause").map(|_| "Paused.".into()).unwrap_or_else(|e| e));
        }
        "play" | "resume" | "play music" | "resume music" | "continue" | "unpause" => {
            return Some(media::control("play").map(|_| "Playing.".into()).unwrap_or_else(|e| e));
        }
        "what's playing" | "whats playing" | "what song is this" | "what is playing" | "which song is this" | "what song is playing" => {
            return Some(media::now_playing().describe());
        }
        _ => {}
    }
    if let Some(q) = t.strip_prefix("play ") {
        let q = q.trim_end_matches(" on spotify").trim();
        if !q.is_empty() && !matches!(q, "music" | "something") {
            return Some(media::play_music(q).map(|_| format!("Opening “{q}” in Spotify.")).unwrap_or_else(|e| e));
        }
    }

    // --- volume
    if has("mute") && !has("unmute") && t.split_whitespace().count() <= 3 {
        return Some(volume::tool(&json!({"mute": true})).unwrap_or_else(|e| e));
    }
    if has("unmute") && t.split_whitespace().count() <= 3 {
        return Some(volume::tool(&json!({"mute": false})).unwrap_or_else(|e| e));
    }
    if has("volume") || has("louder") || has("quieter") || has("softer") {
        if let Some(n) = number(&t) {
            return Some(volume::tool(&json!({"level": n})).unwrap_or_else(|e| e));
        }
        if has("up") || has("louder") || has("increase") || has("raise") {
            return Some(volume::tool(&json!({"change": 10})).unwrap_or_else(|e| e));
        }
        if has("down") || has("quieter") || has("softer") || has("decrease") || has("lower") {
            return Some(volume::tool(&json!({"change": -10})).unwrap_or_else(|e| e));
        }
        if t == "volume" || has("what") {
            return Some(volume::tool(&json!({})).unwrap_or_else(|e| e));
        }
    }

    // --- system
    if matches!(t.as_str(), "lock" | "lock pc" | "lock computer" | "lock screen" | "lock laptop") {
        return Some(system::tool("lock", "").unwrap_or_else(|e| e));
    }
    if matches!(t.as_str(), "what time is it" | "time" | "what's time" | "whats time" | "what is time" | "what day is it" | "what's date" | "what is date" | "date") {
        return Some(format!("It's {}.", system::now_string()));
    }
    if matches!(t.as_str(), "dark mode" | "turn on dark mode" | "switch to dark mode" | "enable dark mode") {
        return Some(system::tool("dark_mode", "").unwrap_or_else(|e| e));
    }
    if matches!(t.as_str(), "light mode" | "turn on light mode" | "switch to light mode" | "enable light mode") {
        return Some(system::tool("light_mode", "").unwrap_or_else(|e| e));
    }
    if matches!(t.as_str(), "show desktop" | "minimize all" | "minimize everything" | "hide all windows") {
        return Some(tools::wins::tool("minimize_all", "").unwrap_or_else(|e| e));
    }
    if matches!(t.as_str(), "screenshot" | "take screenshot" | "take a screenshot" | "snip") {
        return Some(system::tool("screenshot", "").unwrap_or_else(|e| e));
    }

    // --- open folders / apps ("open X", "launch X", "start X")
    for verb in ["open ", "launch ", "start ", "go to "] {
        if let Some(rest) = t.strip_prefix(verb) {
            let target = rest.trim_end_matches(" folder").trim_end_matches(" app").trim();
            // Multi-step requests ("open downloads and delete...") go to the AI.
            if target.is_empty() || target.contains(" and ") || target.contains(" then ") || target.split_whitespace().count() > 4 {
                return None;
            }
            let folder_words = ["downloads", "documents", "desktop", "pictures", "photos", "music", "videos", "screenshots", "home", "recycle bin", "this pc", "settings", "control panel"];
            if folder_words.contains(&target) {
                return Some(files::open(target).unwrap_or_else(|e| e));
            }
            if let Some((name, _)) = tools::apps::find(target) {
                if crate::util::fuzzy_score(&name, target) >= 600 {
                    return Some(tools::apps::open(target).unwrap_or_else(|e| e));
                }
            }
            return None;
        }
    }
    let _ = starts;
    None
}

/// Cheap check used before we have any AI key: would `try_local` handle it?
pub fn looks_local(text: &str) -> bool {
    let t = normalize(text);
    let first = t.split_whitespace().next().unwrap_or("");
    matches!(
        first,
        "next" | "skip" | "previous" | "pause" | "play" | "resume" | "mute" | "unmute" | "volume" | "lock" | "open" | "launch" | "start" | "what's" | "whats" | "what" | "time" | "dark" | "light"
    )
}
