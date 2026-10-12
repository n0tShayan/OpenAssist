//! Playback control through Windows' global media session, the same one
//! behind the volume flyout. Works with Spotify, browsers, VLC, etc. with no
//! logins. Falls back to media keys if no session is exposed.

use super::input::tap_vk;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK};

#[derive(Clone, Default, Debug)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub app: String,
    pub playing: bool,
}

impl MediaInfo {
    pub fn describe(&self) -> String {
        if self.title.is_empty() {
            return "Nothing is playing.".into();
        }
        let by = if self.artist.is_empty() { String::new() } else { format!(" by {}", self.artist) };
        let state = if self.playing { "Playing" } else { "Paused" };
        format!("{state}: {}{by} ({})", self.title, self.app)
    }
}

fn session() -> Option<Session> {
    let mgr = Manager::RequestAsync().ok()?.join().ok()?;
    mgr.GetCurrentSession().ok()
}

fn app_name(aumid: &str) -> String {
    let a = aumid.to_lowercase();
    if a.contains("spotify") {
        "Spotify".into()
    } else if a.contains("chrome") {
        "Chrome".into()
    } else if a.contains("msedge") || a.contains("edge") {
        "Edge".into()
    } else if a.contains("firefox") {
        "Firefox".into()
    } else if a.contains("zune") || a.contains("media") {
        "Media Player".into()
    } else {
        aumid.rsplit(['!', '\\', '.']).next().unwrap_or(aumid).to_string()
    }
}

pub fn now_playing() -> MediaInfo {
    let Some(s) = session() else { return MediaInfo::default() };
    let mut info = MediaInfo::default();
    if let Ok(props) = s.TryGetMediaPropertiesAsync().and_then(|op| op.join()) {
        info.title = props.Title().map(|h| h.to_string()).unwrap_or_default();
        info.artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
    }
    info.app = s.SourceAppUserModelId().map(|h| app_name(&h.to_string())).unwrap_or_default();
    info.playing = s.GetPlaybackInfo().and_then(|p| p.PlaybackStatus()).map(|st| st == Status::Playing).unwrap_or(false);
    info
}

pub fn control(action: &str) -> Result<String, String> {
    if let Some(s) = session() {
        let ok = match action {
            "play" => s.TryPlayAsync().and_then(|o| o.join()),
            "pause" => s.TryPauseAsync().and_then(|o| o.join()),
            "toggle" => s.TryTogglePlayPauseAsync().and_then(|o| o.join()),
            "next" => s.TrySkipNextAsync().and_then(|o| o.join()),
            "previous" => s.TrySkipPreviousAsync().and_then(|o| o.join()),
            _ => return Err(format!("unknown media action {action}")),
        };
        if ok.unwrap_or(false) {
            return Ok(format!("Done ({action})."));
        }
    }
    // No session (or it refused): media keys reach most players anyway.
    let vk = match action {
        "next" => VK_MEDIA_NEXT_TRACK,
        "previous" => VK_MEDIA_PREV_TRACK,
        _ => VK_MEDIA_PLAY_PAUSE,
    };
    tap_vk(vk);
    Ok(format!("Sent the {action} media key."))
}

/// Without a Spotify developer login (and Premium), apps can't start a
/// specific track. Opening Spotify's search with the query is the honest,
/// free option: one click on the top result plays it.
pub fn play_music(query: &str) -> Result<String, String> {
    if query.is_empty() {
        return control("play");
    }
    let q = crate::oauth::urlencode(query);
    if super::apps::is_installed("spotify") && super::shell_open(&format!("spotify:search:{q}")) {
        return Ok(format!(
            "Opened Spotify search for “{query}”. Spotify only lets apps auto-play specific songs with a Premium developer login, so the user may need to click the top result (or ask me to click it)."
        ));
    }
    if super::shell_open(&format!("https://music.youtube.com/search?q={q}")) {
        return Ok(format!("Spotify isn't installed; opened YouTube Music search for “{query}”."));
    }
    Err("couldn't open a music app".into())
}
