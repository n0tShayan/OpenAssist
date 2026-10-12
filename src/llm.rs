//! Free-tier AI providers behind one OpenAI-compatible client.
//!
//! Gemini, Groq and OpenRouter all speak `/chat/completions` with tools, so a
//! request is tried on each configured provider in order and falls through on
//! rate limits (429), outages, or bad keys. Speech-to-text uses Groq Whisper
//! when available, otherwise Gemini's lighter model (a separate free quota).

use crate::config::{self, Config};
use crate::util::{b64_encode, log, truncate};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent("OpenAssist/0.1")
            .build()
            .new_agent()
    })
}

pub struct HttpErr {
    pub status: u16,
    pub body: String,
}

fn post(url: &str, key: &str, body: &Value) -> Result<Value, HttpErr> {
    let mut req = agent().post(url).header("Authorization", &format!("Bearer {key}")).header("Content-Type", "application/json");
    if url.contains("openrouter.ai") {
        req = req.header("HTTP-Referer", "https://github.com/openassist").header("X-Title", "OpenAssist");
    }
    let mut resp = req.send(body.to_string()).map_err(|e| HttpErr { status: 0, body: e.to_string() })?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status >= 400 {
        return Err(HttpErr { status, body: text });
    }
    serde_json::from_str(&text).map_err(|e| HttpErr { status, body: format!("bad JSON: {e}") })
}

pub fn get_json(url: &str, key: Option<&str>) -> Result<Value, String> {
    let mut req = agent().get(url);
    if let Some(k) = key {
        req = req.header("Authorization", &format!("Bearer {k}"));
    }
    let mut resp = req.call().map_err(|e| e.to_string())?;
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub fn post_json_nokey(url: &str, body: &Value) -> Result<Value, String> {
    let mut resp = agent()
        .post(url)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status >= 400 {
        return Err(format!("HTTP {status}: {}", truncate(&text, 200)));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

struct Provider {
    id: &'static str,
    url: &'static str,
    key: String,
    model: String,
}

fn providers(cfg: &Config) -> Vec<Provider> {
    let mut out = Vec::new();
    for id in &cfg.provider_order {
        let key = cfg.key_for(id).to_string();
        if key.is_empty() {
            continue;
        }
        match id.as_str() {
            "gemini" => out.push(Provider {
                id: "Gemini",
                url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
                key,
                model: cfg.gemini_model.clone(),
            }),
            "groq" => out.push(Provider { id: "Groq", url: "https://api.groq.com/openai/v1/chat/completions", key, model: cfg.groq_model.clone() }),
            "openrouter" => {
                let model = if cfg.openrouter_model.is_empty() { pick_openrouter_model(&key) } else { Some(cfg.openrouter_model.clone()) };
                if let Some(model) = model {
                    out.push(Provider { id: "OpenRouter", url: "https://openrouter.ai/api/v1/chat/completions", key, model });
                }
            }
            _ => {}
        }
    }
    out
}

/// Picks a free model and remembers it.
fn pick_openrouter_model(key: &str) -> Option<String> {
    let model = free_openrouter_model(Some(key))?;
    let m2 = model.clone();
    config::update(|c| c.openrouter_model = m2);
    Some(model)
}

/// The free (`$0`) model with tool calling and the largest context window.
pub fn free_openrouter_model(key: Option<&str>) -> Option<String> {
    let v = get_json("https://openrouter.ai/api/v1/models", key).ok()?;
    let mut best: Option<(u64, String)> = None;
    for m in v.get("data")?.as_array()? {
        let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
        let free = |k: &str| m.pointer(&format!("/pricing/{k}")).and_then(Value::as_str).map(|p| p.parse::<f64>().unwrap_or(1.0) == 0.0).unwrap_or(false);
        let tools = m
            .get("supported_parameters")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|p| p.as_str() == Some("tools")))
            .unwrap_or(false);
        if !(free("prompt") && free("completion") && tools) || !id.ends_with(":free") {
            continue;
        }
        let ctx = m.get("context_length").and_then(Value::as_u64).unwrap_or(0);
        if best.as_ref().map(|(c, _)| ctx > *c).unwrap_or(true) {
            best = Some((ctx, id.to_string()));
        }
    }
    best.map(|b| b.1)
}

pub struct ChatResult {
    pub message: Value,
    pub provider: &'static str,
}

fn friendly(provider: &str, e: &HttpErr) -> String {
    match e.status {
        0 => format!("{provider}: can't reach the server ({}).", truncate(&e.body, 80)),
        401 | 403 => format!("{provider} rejected the key. Check it in Settings."),
        400 if e.body.contains("API key") || e.body.contains("API_KEY_INVALID") => {
            format!("{provider} rejected the key. Check it in Settings.")
        }
        429 => format!("{provider}'s free quota is used up for now."),
        400 if e.body.contains("location is not supported") || e.body.contains("User location") => {
            format!("{provider} isn't available in your region.")
        }
        s => format!("{provider} error {s}: {}", truncate(&e.body, 160)),
    }
}

/// One chat turn with tool definitions; falls through providers on failure.
pub fn chat(messages: &[Value], tools: &Value) -> Result<ChatResult, String> {
    let cfg = config::get();
    let list = providers(&cfg);
    if list.is_empty() {
        return Err("No AI provider is connected. Open Settings from the island or tray.".into());
    }
    let mut errors = Vec::new();
    for p in list {
        let mut body = json!({
            "model": p.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": 0.3,
            "max_tokens": 700,
        });
        if p.id == "Groq" {
            // gpt-oss: keep reasoning short to save quota and latency.
            body["reasoning_effort"] = json!("low");
        }
        match post(p.url, &p.key, &body) {
            Ok(v) => {
                if let Some(msg) = v.pointer("/choices/0/message") {
                    return Ok(ChatResult { message: msg.clone(), provider: p.id });
                }
                errors.push(format!("{}: empty response", p.id));
            }
            Err(e) => {
                log(&format!("{} failed: {} {}", p.id, e.status, truncate(&e.body, 300)));
                // A model that can't do tools on this provider: try once without.
                if e.status == 400 && e.body.contains("tool") && p.id == "OpenRouter" {
                    config::update(|c| c.openrouter_model.clear());
                }
                errors.push(friendly(p.id, &e));
            }
        }
    }
    Err(errors.join(" "))
}

/// Speech to text. Returns the transcript (may be empty).
pub fn transcribe(wav: &[u8]) -> Result<String, String> {
    let cfg = config::get();
    let mut errors = Vec::new();
    if !cfg.groq_key.is_empty() {
        match groq_whisper(&cfg, wav) {
            Ok(t) => return Ok(t),
            Err(e) => errors.push(e),
        }
    }
    if !cfg.gemini_key.is_empty() {
        let body = json!({
            "model": cfg.gemini_stt_model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "Transcribe this voice command exactly as spoken. Reply with only the transcript, nothing else. If there is no speech, reply with an empty string."},
                    {"type": "input_audio", "input_audio": {"data": b64_encode(wav), "format": "wav"}}
                ]
            }],
            "temperature": 0,
        });
        match post("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", &cfg.gemini_key, &body) {
            Ok(v) => {
                let t = v.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or_default();
                return Ok(t.trim().trim_matches('"').to_string());
            }
            Err(e) => errors.push(friendly("Gemini", &e)),
        }
    }
    if errors.is_empty() {
        return Err("Voice needs a free Gemini or Groq key.".into());
    }
    Err(errors.join(" "))
}

fn groq_whisper(cfg: &Config, wav: &[u8]) -> Result<String, String> {
    let boundary = format!("----openassist{:x}", u64::from_le_bytes(crate::util::random_bytes::<8>()));
    let mut body: Vec<u8> = Vec::with_capacity(wav.len() + 512);
    let field = |b: &mut Vec<u8>, name: &str, val: &str| {
        b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{val}\r\n").as_bytes());
    };
    field(&mut body, "model", &cfg.groq_stt_model);
    field(&mut body, "response_format", "json");
    field(&mut body, "temperature", "0");
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes());
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let mut resp = agent()
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", &format!("Bearer {}", cfg.groq_key))
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send(&body[..])
        .map_err(|e| format!("Groq: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status >= 400 {
        return Err(friendly("Groq", &HttpErr { status, body: text }));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(v.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string())
}
