//! Spoken replies through Windows' own speech synthesiser (the OneCore
//! voices installed with Windows). No model ships with the app.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{MediaPlaybackState, MediaPlayer};
use windows::Media::SpeechSynthesis::SpeechSynthesizer;
use windows::core::HSTRING;

static PLAYER: Mutex<Option<MediaPlayer>> = Mutex::new(None);
static GENERATION: Mutex<u64> = Mutex::new(0);

pub fn speak(text: &str) {
    let text = clean(text);
    if text.is_empty() {
        return;
    }
    stop();
    let generation = {
        let mut g = GENERATION.lock().unwrap();
        *g += 1;
        *g
    };
    std::thread::spawn(move || {
        let _com = crate::tools::ComGuard::mta();
        if let Err(e) = play(&text, generation) {
            crate::util::log(&format!("tts: {e:?}"));
        }
    });
}

fn play(text: &str, generation: u64) -> windows::core::Result<()> {
    let synth = SpeechSynthesizer::new()?;
    let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text))?.join()?;
    if *GENERATION.lock().unwrap() != generation {
        return Ok(()); // superseded while synthesising
    }
    let ctype = stream.ContentType()?;
    let source = MediaSource::CreateFromStream(&stream, &ctype)?;
    let player = MediaPlayer::new()?;
    player.SetSource(&source)?;
    player.Play()?;
    *PLAYER.lock().unwrap() = Some(player.clone());
    // Wait for playback to finish, then let the player go.
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    loop {
        std::thread::sleep(Duration::from_millis(150));
        if *GENERATION.lock().unwrap() != generation || start.elapsed() > Duration::from_secs(90) {
            break;
        }
        let state = player.PlaybackSession().and_then(|s| s.PlaybackState());
        match state {
            Ok(MediaPlaybackState::Playing) | Ok(MediaPlaybackState::Opening) | Ok(MediaPlaybackState::Buffering) => continue,
            _ => break,
        }
    }
    let current = *GENERATION.lock().unwrap();
    if current == generation {
        *PLAYER.lock().unwrap() = None;
    }
    Ok(())
}

pub fn stop() {
    *GENERATION.lock().unwrap() += 1;
    if let Some(p) = PLAYER.lock().unwrap().take() {
        let _ = p.Pause();
        let _ = p.Close();
    }
}

/// Strip markdown-ish noise so the voice doesn't read symbols aloud.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '_' | '⏰'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Synthesises without playing; reports the voice and audio size.
pub fn synth_test(text: &str) -> String {
    let run = || -> windows::core::Result<String> {
        let synth = SpeechSynthesizer::new()?;
        let voice = synth.Voice()?.DisplayName()?;
        let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text))?.join()?;
        Ok(format!("voice: {voice}, audio: {} bytes, {} voices installed", stream.Size()?, SpeechSynthesizer::AllVoices()?.Size()?))
    };
    run().unwrap_or_else(|e| format!("tts error: {e:?}"))
}
