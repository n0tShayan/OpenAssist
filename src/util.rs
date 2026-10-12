//! Small, dependency-free helpers: wide strings, base64, SHA-256/random via
//! Windows CNG, DPAPI secret storage, and opt-in logging.

use std::path::PathBuf;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    BCRYPT_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, BCryptHash,
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::{PCWSTR, w};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Keeps the buffer alive alongside the pointer.
pub struct WStr(Vec<u16>);
impl WStr {
    pub fn new(s: &str) -> Self {
        WStr(wide(s))
    }
    pub fn pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

// ---------------------------------------------------------------- base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn b64url_encode(data: &[u8]) -> String {
    b64_encode(data).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' => continue,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- crypto (CNG)

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    unsafe {
        let _ = BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out);
    }
    out
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    unsafe {
        let _ = BCryptGenRandom(None, &mut out, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
    out
}

// ---------------------------------------------------------------- DPAPI

/// Encrypts a secret for the current Windows user. Returns base64.
pub fn protect(secret: &str) -> Option<String> {
    if secret.is_empty() {
        return Some(String::new());
    }
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: secret.len() as u32, pbData: secret.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, w!("OpenAssist"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).ok()?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as _)));
        Some(b64_encode(&bytes))
    }
}

pub fn unprotect(b64: &str) -> Option<String> {
    if b64.is_empty() {
        return Some(String::new());
    }
    let mut bytes = b64_decode(b64)?;
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_mut_ptr() };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).ok()?;
        let plain = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as _)));
        String::from_utf8(plain).ok()
    }
}

// ---------------------------------------------------------------- paths & logging

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("OpenAssist")
}

/// Logging is off unless OPENASSIST_LOG is set, so the app never touches the
/// disk on its own.
pub fn log(msg: &str) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("OPENASSIST_LOG").is_some()) {
        return;
    }
    use std::io::Write;
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir().join("log.txt")) {
        let _ = writeln!(f, "{msg}");
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Case-insensitive fuzzy score: 0 = no match, higher = better.
pub fn fuzzy_score(hay: &str, needle: &str) -> i32 {
    let h = hay.to_lowercase();
    let n = needle.to_lowercase();
    let n = n.trim();
    if n.is_empty() {
        return 0;
    }
    if h == n {
        return 1000;
    }
    if h.starts_with(n) {
        return 800 - (h.len() as i32 - n.len() as i32).min(200);
    }
    if h.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(n)) {
        return 600 - (h.len() as i32).min(200);
    }
    if h.contains(n) {
        return 400 - (h.len() as i32).min(200);
    }
    // all needle words present
    let words: Vec<&str> = n.split_whitespace().collect();
    if words.len() > 1 && words.iter().all(|w| h.contains(w)) {
        return 300;
    }
    0
}
