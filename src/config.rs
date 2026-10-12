//! Settings live in %APPDATA%\OpenAssist\config.json and are written only when
//! something changes. API keys are DPAPI-encrypted for the current user.

use crate::util::{data_dir, protect, unprotect};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
pub struct Config {
    pub gemini_key: String,
    pub groq_key: String,
    pub openrouter_key: String,
    /// Tried in this order; providers without a key are skipped.
    pub provider_order: Vec<String>,
    pub gemini_model: String,
    pub gemini_stt_model: String,
    pub groq_model: String,
    pub groq_stt_model: String,
    /// Empty = pick a free tool-capable model automatically.
    pub openrouter_model: String,
    pub voice_replies: bool,
    pub hide_in_fullscreen: bool,
    /// Click/type into other apps via UI Automation.
    pub allow_ui_control: bool,
    pub max_steps: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            gemini_key: String::new(),
            groq_key: String::new(),
            openrouter_key: String::new(),
            provider_order: vec!["gemini".into(), "groq".into(), "openrouter".into()],
            gemini_model: "gemini-flash-latest".into(),
            gemini_stt_model: "gemini-flash-lite-latest".into(),
            groq_model: "openai/gpt-oss-120b".into(),
            groq_stt_model: "whisper-large-v3-turbo".into(),
            openrouter_model: String::new(),
            voice_replies: true,
            hide_in_fullscreen: true,
            allow_ui_control: true,
            max_steps: 6,
        }
    }
}

impl Config {
    pub fn key_for(&self, provider: &str) -> &str {
        match provider {
            "gemini" => &self.gemini_key,
            "groq" => &self.groq_key,
            "openrouter" => &self.openrouter_key,
            _ => "",
        }
    }

    pub fn has_any_key(&self) -> bool {
        !(self.gemini_key.is_empty() && self.groq_key.is_empty() && self.openrouter_key.is_empty())
    }

    pub fn can_hear(&self) -> bool {
        !self.groq_key.is_empty() || !self.gemini_key.is_empty()
    }

    fn load() -> Config {
        let mut c = Config::default();
        let Ok(text) = std::fs::read_to_string(data_dir().join("config.json")) else { return c };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return c };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let b = |k: &str| v.get(k).and_then(Value::as_bool);
        let key = |k: &str| s(k).and_then(|e| unprotect(&e)).unwrap_or_default();
        c.gemini_key = key("gemini_key");
        c.groq_key = key("groq_key");
        c.openrouter_key = key("openrouter_key");
        if let Some(order) = v.get("provider_order").and_then(Value::as_array) {
            let o: Vec<String> = order.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
            if !o.is_empty() {
                c.provider_order = o;
            }
        }
        if let Some(x) = s("gemini_model") { c.gemini_model = x; }
        if let Some(x) = s("gemini_stt_model") { c.gemini_stt_model = x; }
        if let Some(x) = s("groq_model") { c.groq_model = x; }
        if let Some(x) = s("groq_stt_model") { c.groq_stt_model = x; }
        if let Some(x) = s("openrouter_model") { c.openrouter_model = x; }
        if let Some(x) = b("voice_replies") { c.voice_replies = x; }
        if let Some(x) = b("hide_in_fullscreen") { c.hide_in_fullscreen = x; }
        if let Some(x) = b("allow_ui_control") { c.allow_ui_control = x; }
        if let Some(x) = v.get("max_steps").and_then(Value::as_u64) { c.max_steps = (x as usize).clamp(1, 12); }
        c
    }

    fn save(&self) {
        let v = json!({
            "gemini_key": protect(&self.gemini_key).unwrap_or_default(),
            "groq_key": protect(&self.groq_key).unwrap_or_default(),
            "openrouter_key": protect(&self.openrouter_key).unwrap_or_default(),
            "provider_order": self.provider_order,
            "gemini_model": self.gemini_model,
            "gemini_stt_model": self.gemini_stt_model,
            "groq_model": self.groq_model,
            "groq_stt_model": self.groq_stt_model,
            "openrouter_model": self.openrouter_model,
            "voice_replies": self.voice_replies,
            "hide_in_fullscreen": self.hide_in_fullscreen,
            "allow_ui_control": self.allow_ui_control,
            "max_steps": self.max_steps,
        });
        let _ = std::fs::create_dir_all(data_dir());
        let tmp = data_dir().join("config.json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, data_dir().join("config.json"));
        }
    }
}

fn store() -> &'static Mutex<Config> {
    static CFG: OnceLock<Mutex<Config>> = OnceLock::new();
    CFG.get_or_init(|| Mutex::new(Config::load()))
}

pub fn get() -> Config {
    store().lock().unwrap().clone()
}

/// Mutate and persist in one step.
pub fn update(f: impl FnOnce(&mut Config)) {
    let mut c = store().lock().unwrap();
    f(&mut c);
    c.save();
}
