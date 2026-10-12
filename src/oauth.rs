//! One-click OpenRouter sign-in (OAuth PKCE). The browser redirects to a
//! throwaway listener on localhost:3000, we swap the code for a user-owned
//! API key, and the listener closes. Nothing to copy or paste.

use crate::events::{self, UiEvent};
use crate::util::{b64url_encode, random_bytes, sha256};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const PORT: u16 = 3000;

pub fn connect_openrouter() {
    std::thread::spawn(|| {
        let msg = match run() {
            Ok(()) => "OpenRouter connected ✓ — free models are ready.".to_string(),
            Err(e) => format!("OpenRouter sign-in didn't finish: {e}"),
        };
        events::post(UiEvent::ConfigChanged(msg));
    });
}

fn run() -> Result<(), String> {
    let verifier = b64url_encode(&random_bytes::<32>());
    let challenge = b64url_encode(&sha256(verifier.as_bytes()));

    // Browsers may resolve "localhost" to either stack; listen on both.
    let listeners: Vec<TcpListener> = ["127.0.0.1", "[::1]"]
        .iter()
        .filter_map(|h| TcpListener::bind(format!("{h}:{PORT}")).ok())
        .collect();
    if listeners.is_empty() {
        return Err(format!("port {PORT} is busy — close the app using it and try again"));
    }
    for l in &listeners {
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
    }

    let callback = format!("http://localhost:{PORT}/openassist");
    let url = format!(
        "https://openrouter.ai/auth?callback_url={}&code_challenge={challenge}&code_challenge_method=S256",
        urlencode(&callback)
    );
    crate::tools::shell_open(&url);

    let deadline = Instant::now() + Duration::from_secs(300);
    let code = 'outer: loop {
        if Instant::now() > deadline {
            return Err("timed out".into());
        }
        for l in &listeners {
            if let Ok((mut s, _)) = l.accept() {
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("");
                let code = query_param(path, "code");
                let page = if code.is_some() {
                    "<h2 style='font-family:Segoe UI'>OpenAssist is connected ✓</h2><p style='font-family:Segoe UI'>You can close this tab.</p>"
                } else {
                    "<p style='font-family:Segoe UI'>Waiting for OpenRouter…</p>"
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
                let _ = s.write_all(resp.as_bytes());
                if let Some(c) = code {
                    break 'outer c;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    drop(listeners);

    let v: Value = crate::llm::post_json_nokey(
        "https://openrouter.ai/api/v1/auth/keys",
        &json!({"code": code, "code_verifier": verifier, "code_challenge_method": "S256"}),
    )?;
    let key = v.get("key").and_then(Value::as_str).ok_or("no key in response")?.to_string();
    crate::config::update(|c| {
        c.openrouter_key = key;
        c.openrouter_model.clear();
    });
    Ok(())
}

fn query_param(path: &str, name: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
