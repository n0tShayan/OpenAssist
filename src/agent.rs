//! The agent loop: local fast path first, then the model with tools, capped
//! at a few steps so one request can't burn the free daily quota.

use crate::events::{self, UiEvent};
use crate::{config, intents, llm, tools};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static BUSY: AtomicBool = AtomicBool::new(false);
/// Short rolling memory of the conversation (text turns only), forgotten
/// after 10 idle minutes.
static HISTORY: Mutex<(Option<Instant>, Vec<Value>)> = Mutex::new((None, Vec::new()));

pub fn is_busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

struct BusyGuard;
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// Called on the mic thread once recording stops.
pub fn handle_voice(wav: Vec<u8>) {
    if BUSY.swap(true, Ordering::SeqCst) {
        events::post(UiEvent::Error("Still working on the last request…".into()));
        return;
    }
    let _g = BusyGuard;
    let _com = tools::ComGuard::mta();
    match llm::transcribe(&wav) {
        Ok(text) if is_real_speech(&text) => run(text),
        Ok(_) => events::post(UiEvent::Idle),
        Err(e) => events::post(UiEvent::Error(e)),
    }
}

/// Typed commands. Returns false if busy.
pub fn submit_text(text: String, _spoken: bool) -> bool {
    if BUSY.swap(true, Ordering::SeqCst) {
        return false;
    }
    std::thread::Builder::new()
        .name("agent".into())
        .spawn(move || {
            let _g = BusyGuard;
            let _com = tools::ComGuard::mta();
            run(text);
        })
        .is_ok()
}

/// Whisper-style models invent these on silence or noise.
fn is_real_speech(t: &str) -> bool {
    let n = t.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    if n.len() < 2 {
        return false;
    }
    !matches!(
        n.as_str(),
        "thank you" | "thanks for watching" | "thank you for watching" | "you" | "bye" | "thanks" | "subscribe" | "so" | "uh" | "um" | "hmm"
    )
}

fn system_prompt() -> String {
    let fg = tools::user_window()
        .map(|h| {
            let mut buf = [0u16; 200];
            let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(h, &mut buf) };
            crate::util::from_wide(&buf[..n.max(0) as usize])
        })
        .unwrap_or_default();
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(
        "You are OpenAssist, a voice assistant that operates the user's Windows PC. \
Replies are shown in a small notch and often spoken: 1-2 short sentences, plain text, no markdown or lists. \
Act with tools; never claim you did something a tool didn't confirm. Use as few tool calls as possible. \
Folder names like downloads/documents/desktop work as paths. If a tool fails, say what happened plainly. \
Windows blocks control of admin windows, UAC prompts, the lock screen and some games; say so when relevant. \
You can't run terminal commands. Now: {}. Windows user: {user}. User's active window: \"{}\".",
        tools::system::now_string(),
        crate::util::truncate(&fg, 80)
    )
}

fn run(text: String) {
    events::post(UiEvent::Heard(text.clone()));

    if let Some(reply) = intents::try_local(&text) {
        remember(&text, &reply);
        events::post(UiEvent::Reply(reply));
        tools::apps::forget();
        return;
    }

    let cfg = config::get();
    if !cfg.has_any_key() {
        events::post(UiEvent::Error("I can do simple commands offline, but this one needs an AI provider. Connect one in Settings (it's free).".into()));
        return;
    }

    let tools_schema = tools::schemas(cfg.allow_ui_control);
    let mut messages = vec![json!({"role": "system", "content": system_prompt()})];
    {
        let mut h = HISTORY.lock().unwrap();
        if h.0.map(|t| t.elapsed() > Duration::from_secs(600)).unwrap_or(false) {
            h.1.clear();
        }
        messages.extend(h.1.iter().cloned());
    }
    messages.push(json!({"role": "user", "content": text}));

    let mut last_provider = "";
    for step in 0..cfg.max_steps {
        let res = match llm::chat(&messages, &tools_schema) {
            Ok(r) => r,
            Err(e) => {
                events::post(UiEvent::Error(e));
                return;
            }
        };
        last_provider = res.provider;
        let msg = res.message;
        let calls = msg.get("tool_calls").and_then(Value::as_array).cloned().unwrap_or_default();
        if calls.is_empty() {
            let reply = msg.get("content").and_then(Value::as_str).unwrap_or("").trim().to_string();
            let reply = if reply.is_empty() { "Done.".to_string() } else { reply };
            remember(&text, &reply);
            events::post(UiEvent::Reply(reply));
            tools::apps::forget();
            return;
        }
        // Echo the assistant turn back with only the fields providers accept.
        let mut assistant = json!({"role": "assistant", "tool_calls": calls});
        assistant["content"] = msg.get("content").cloned().filter(|c| !c.is_null()).unwrap_or(json!(""));
        messages.push(assistant);
        for call in &calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("call").to_string();
            let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string();
            let raw = call.pointer("/function/arguments").cloned().unwrap_or(json!("{}"));
            let args: Value = match &raw {
                Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                v => v.clone(),
            };
            events::post(UiEvent::Thinking(tools::label(&name, &args)));
            crate::util::log(&format!("step {step}: {name} {args}"));
            let result = tools::execute(&name, &args);
            crate::util::log(&format!("  -> {}", crate::util::truncate(&result, 200)));
            messages.push(json!({"role": "tool", "tool_call_id": id, "name": name, "content": result}));
        }
        events::post(UiEvent::Thinking("Thinking…".into()));
    }
    let _ = last_provider;
    events::post(UiEvent::Reply("I did what I could in the steps I allow myself per request. Ask me to continue if needed.".into()));
}

fn remember(user: &str, reply: &str) {
    let mut h = HISTORY.lock().unwrap();
    h.0 = Some(Instant::now());
    h.1.push(json!({"role": "user", "content": user}));
    h.1.push(json!({"role": "assistant", "content": reply}));
    let n = h.1.len();
    if n > 8 {
        h.1.drain(..n - 8);
    }
}
