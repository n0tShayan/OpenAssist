//! A minimal single-line text field drawn with DirectWrite: typing, caret
//! movement, word delete, select-all, paste, and masking for secrets.

use crate::gfx::{Align, Canvas, Color, Font, RectF};
use windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS;

#[derive(Default, Clone)]
pub struct TextBox {
    pub chars: Vec<char>,
    pub caret: usize,
    pub all_selected: bool,
    pub masked: bool,
    scroll: f32,
}

pub enum Key {
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    WordBackspace,
    SelectAll,
}

impl TextBox {
    pub fn masked() -> Self {
        TextBox { masked: true, ..Default::default() }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.caret = self.chars.len();
        self.all_selected = false;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    fn take_selection(&mut self) {
        if self.all_selected {
            self.chars.clear();
            self.caret = 0;
            self.all_selected = false;
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.take_selection();
        for ch in s.chars().filter(|c| !c.is_control()) {
            self.chars.insert(self.caret, ch);
            self.caret += 1;
        }
    }

    pub fn key(&mut self, k: Key) {
        match k {
            Key::SelectAll => {
                self.all_selected = !self.chars.is_empty();
                return;
            }
            Key::Backspace | Key::Delete | Key::WordBackspace if self.all_selected => {
                self.take_selection();
                return;
            }
            _ => {}
        }
        self.all_selected = false;
        match k {
            Key::Left => self.caret = self.caret.saturating_sub(1),
            Key::Right => self.caret = (self.caret + 1).min(self.chars.len()),
            Key::Home => self.caret = 0,
            Key::End => self.caret = self.chars.len(),
            Key::Backspace => {
                if self.caret > 0 {
                    self.caret -= 1;
                    self.chars.remove(self.caret);
                }
            }
            Key::Delete => {
                if self.caret < self.chars.len() {
                    self.chars.remove(self.caret);
                }
            }
            Key::WordBackspace => {
                let mut i = self.caret;
                while i > 0 && self.chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !self.chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                self.chars.drain(i..self.caret);
                self.caret = i;
            }
            Key::SelectAll => {}
        }
    }

    fn display(&self) -> String {
        if self.masked {
            // Show a short tail so people can recognise which key it is.
            let n = self.chars.len();
            if n > 6 {
                let tail: String = self.chars[n - 4..].iter().collect();
                return format!("{}{}", "•".repeat((n - 4).min(24)), tail);
            }
            return "•".repeat(n);
        }
        self.text()
    }

    /// Draws the field contents (not the background) clipped to `r`.
    pub fn draw(&mut self, c: &mut Canvas, r: RectF, size: f32, color: Color, placeholder: &str, ph_color: Color, focused: bool, caret_on: bool, accent: Color) {
        let shown = self.display();
        if shown.is_empty() {
            c.text(placeholder, r, Font::Text, size, 400, Align::Left, ph_color);
            if focused && caret_on {
                c.fill_rect(RectF::new(r.x, r.cy() - size * 0.62, 1.6, size * 1.24), 0.8, accent);
            }
            return;
        }
        let layout = c.g.layout(&shown, Font::Text, size, 400, Align::Left, false, 10_000.0, r.h);
        // Caret index in display string (masking keeps a 1:1 char count except
        // when truncated, so clamp).
        let disp_len = shown.encode_utf16().count() as u32;
        let caret_u16: u32 = if self.masked {
            disp_len
        } else {
            self.chars[..self.caret].iter().map(|c| c.len_utf16() as u32).sum()
        };
        let (mut cx, mut _cy) = (0f32, 0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = layout.HitTestTextPosition(caret_u16.min(disp_len), false, &mut cx, &mut _cy, &mut m);
        }
        let mut full = DWRITE_HIT_TEST_METRICS::default();
        let (mut ex, mut _ey) = (0f32, 0f32);
        unsafe {
            let _ = layout.HitTestTextPosition(disp_len, false, &mut ex, &mut _ey, &mut full);
        }
        // keep caret visible
        if cx - self.scroll > r.w - 4.0 {
            self.scroll = cx - r.w + 4.0;
        }
        if cx - self.scroll < 0.0 {
            self.scroll = cx;
        }
        self.scroll = self.scroll.min((ex - r.w + 4.0).max(0.0)).max(0.0);
        if self.all_selected {
            c.fill_rect(RectF::new(r.x - self.scroll, r.cy() - size * 0.7, ex.min(r.w + self.scroll), size * 1.4), 3.0, Color { a: 0.35, ..accent });
        }
        c.draw_layout(&layout, r.x - self.scroll, r.y, color);
        if focused && caret_on && !self.all_selected {
            c.fill_rect(RectF::new(r.x + cx - self.scroll, r.cy() - size * 0.62, 1.6, size * 1.24), 0.8, accent);
        }
    }
}
