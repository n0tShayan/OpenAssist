//! Microphone capture with WASAPI. The mic is opened only while you talk;
//! Windows converts to 16 kHz mono 16-bit for us (AUTOCONVERTPCM), which is
//! exactly what speech-to-text wants, so there is no resampler here.
//!
//! When recording stops, the same thread hands the audio to the agent.

use crate::events::{self, UiEvent};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

const RATE: u32 = 16_000;
const MAX_SECS: u64 = 30;

pub struct Recording {
    stop: Arc<AtomicBool>,
    auto_stop: Arc<AtomicBool>,
}

impl Recording {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
    /// Hands-free: stop by itself after the user pauses.
    pub fn set_auto_stop(&self, on: bool) {
        self.auto_stop.store(on, Ordering::SeqCst);
    }
}

pub fn start() -> Result<Recording, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let auto_stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (s2, a2) = (stop.clone(), auto_stop.clone());
    std::thread::Builder::new()
        .name("mic".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let _com = crate::tools::ComGuard::mta();
            match capture(&s2, &a2, &tx) {
                Ok(pcm) => finish(pcm),
                Err(e) => {
                    let _ = tx.send(Err(e.clone()));
                    events::post(UiEvent::Error(format!("Microphone error: {e}")));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(Recording { stop, auto_stop }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the microphone didn't respond".into()),
    }
}

fn capture(stop: &AtomicBool, auto_stop: &AtomicBool, ready: &std::sync::mpsc::Sender<Result<(), String>>) -> Result<Vec<i16>, String> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.message())?;
        let dev = en.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|_| "no microphone found".to_string())?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(|e| e.message())?;
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                2_000_000, // 200 ms buffer, in 100ns units
                0,
                &fmt,
                None,
            )
            .map_err(|e| e.message())?;
        let cap: IAudioCaptureClient = client.GetService().map_err(|e| e.message())?;
        client.Start().map_err(|e| e.message())?;
        let _ = ready.send(Ok(()));

        let mut pcm: Vec<i16> = Vec::with_capacity(RATE as usize * 8);
        let started = Instant::now();
        let mut noise = 0.012f32; // adaptive noise floor
        let mut heard_speech = false;
        let mut last_voice = Instant::now();
        let mut last_post = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let mut chunk_peak = 0f32;
            let mut chunk_sum = 0f64;
            let mut chunk_n = 0usize;
            while cap.GetNextPacketSize().unwrap_or(0) > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                if cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                    break;
                }
                let n = frames as usize;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    pcm.extend(std::iter::repeat_n(0i16, n));
                } else {
                    let s = std::slice::from_raw_parts(data as *const i16, n);
                    for &v in s {
                        let f = v as f32 / 32768.0;
                        chunk_sum += (f * f) as f64;
                        chunk_peak = chunk_peak.max(f.abs());
                    }
                    chunk_n += n;
                    pcm.extend_from_slice(s);
                }
                let _ = cap.ReleaseBuffer(frames);
            }
            if chunk_n > 0 {
                let rms = (chunk_sum / chunk_n as f64).sqrt() as f32;
                if started.elapsed() < Duration::from_millis(250) || rms < noise {
                    noise = noise * 0.9 + rms * 0.1;
                }
                let threshold = (noise * 2.8).max(0.018);
                if rms > threshold {
                    heard_speech = true;
                    last_voice = Instant::now();
                }
                if last_post.elapsed() >= Duration::from_millis(33) {
                    let level = ((rms / 0.12).sqrt()).min(1.0);
                    events::post(UiEvent::Level(level));
                    last_post = Instant::now();
                }
                let _ = chunk_peak;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if auto_stop.load(Ordering::SeqCst) {
                if heard_speech && last_voice.elapsed() > Duration::from_millis(1100) {
                    break;
                }
                if !heard_speech && started.elapsed() > Duration::from_secs(7) {
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(MAX_SECS) {
                break;
            }
        }
        let _ = client.Stop();
        if !heard_speech {
            pcm.clear();
        }
        Ok(pcm)
    }
}

fn finish(pcm: Vec<i16>) {
    if pcm.len() < (RATE as usize * 35 / 100) {
        events::post(UiEvent::Idle);
        return;
    }
    events::post(UiEvent::Transcribing);
    crate::agent::handle_voice(wav(&pcm));
}

pub fn wav(pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
