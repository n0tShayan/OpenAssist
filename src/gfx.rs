//! Direct2D (software) + DirectWrite drawing onto a per-pixel-alpha layered
//! window. No GPU device and no swap chain: a frame is rasterised into a DIB,
//! handed to DWM with UpdateLayeredWindow, and the DIB is freed again once the
//! UI is static. DWM keeps showing the last frame at zero cost to us.

use std::collections::HashMap;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{PCWSTR, Result};
use windows_numerics::{Matrix3x2, Vector2};

pub type Color = D2D1_COLOR_F;

pub const fn rgba(hex: u32, a: f32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    Color { a: c.a * a, ..c }
}

pub fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        RectF { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    pub fn inset(&self, dx: f32, dy: f32) -> RectF {
        RectF::new(self.x + dx, self.y + dy, self.w - 2.0 * dx, self.h - 2.0 * dy)
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Font {
    Text,
    Display,
    Icons,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A memory DC + 32bpp top-down DIB the size of the window.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    rt: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    surface: Option<Surface>,
    formats: HashMap<(Font, u32, i32, Align, bool), IDWriteTextFormat>,
    pub scale: f32,
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Gfx { d2d, dw, rt: None, brush: None, surface: None, formats: HashMap::new(), scale: 1.0 })
        }
    }

    /// Releases the pixel buffer and render target. Called when the UI goes
    /// static so nothing frame-sized stays resident.
    pub fn release(&mut self) {
        self.brush = None;
        self.rt = None;
        self.surface = None;
    }

    fn ensure(&mut self, w: i32, h: i32) -> Result<()> {
        let need = match &self.surface {
            Some(s) => s.w != w || s.h != h,
            None => true,
        };
        if need {
            self.surface = None;
            unsafe {
                let screen = GetDC(None);
                let dc = CreateCompatibleDC(Some(screen));
                ReleaseDC(None, screen);
                let bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits = std::ptr::null_mut();
                let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
                let old = SelectObject(dc, bmp.into());
                self.surface = Some(Surface { dc, bmp, old, w, h });
            }
        }
        if self.rt.is_none() {
            unsafe {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = self.d2d.CreateDCRenderTarget(&props)?;
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                self.brush = Some(rt.CreateSolidColorBrush(&rgba(0xffffff, 1.0), None)?);
                self.rt = Some(rt);
            }
        }
        Ok(())
    }

    /// Renders one frame of `w`x`h` physical pixels and presents it at
    /// `(x, y)` on screen. `draw` works in DIPs.
    pub fn frame(&mut self, hwnd: HWND, x: i32, y: i32, w: i32, h: i32, draw: impl FnOnce(&mut Canvas)) -> Result<()> {
        self.ensure(w, h)?;
        let rt = self.rt.clone().unwrap();
        let surface = self.surface.as_ref().unwrap();
        let dc = surface.dc;
        unsafe {
            rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            rt.SetDpi(96.0 * self.scale, 96.0 * self.scale);
            rt.BeginDraw();
            rt.Clear(Some(&rgba(0, 0.0)));
            {
                let mut c = Canvas { g: self, rt: rt.clone() };
                draw(&mut c);
            }
            let _ = rt.EndDraw(None, None);
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let dst = POINT { x, y };
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            UpdateLayeredWindow(hwnd, None, Some(&dst), Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA)?;
        }
        Ok(())
    }

    pub fn format(&mut self, font: Font, size: f32, weight: i32, align: Align, wrap: bool) -> IDWriteTextFormat {
        let key = (font, (size * 10.0) as u32, weight, align, wrap);
        if let Some(f) = self.formats.get(&key) {
            return f.clone();
        }
        let family = match font {
            Font::Text => "Segoe UI Variable Text",
            Font::Display => "Segoe UI Variable Display",
            Font::Icons => {
                if font_exists(&self.dw, "Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" }
            }
        };
        let fam = crate::util::wide(family);
        let loc = crate::util::wide("en-us");
        let f = unsafe {
            self.dw
                .CreateTextFormat(
                    PCWSTR(fam.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT(weight),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(loc.as_ptr()),
                )
                .unwrap()
        };
        unsafe {
            let _ = f.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            if !wrap {
                let sign = self.dw.CreateEllipsisTrimmingSign(&f).ok();
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = f.SetTrimming(&trim, sign.as_ref());
            }
        }
        self.formats.insert(key, f.clone());
        f
    }

    pub fn layout(&mut self, text: &str, font: Font, size: f32, weight: i32, align: Align, wrap: bool, max_w: f32, max_h: f32) -> IDWriteTextLayout {
        let f = self.format(font, size, weight, align, wrap);
        let t: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dw.CreateTextLayout(&t, &f, max_w.max(1.0), max_h.max(1.0)).unwrap() }
    }

    /// Height of wrapped text in DIPs.
    pub fn measure(&mut self, text: &str, font: Font, size: f32, weight: i32, max_w: f32) -> (f32, f32) {
        let l = self.layout(text, font, size, weight, Align::Left, true, max_w, 10_000.0);
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        (m.widthIncludingTrailingWhitespace, m.height)
    }
}

fn font_exists(dw: &IDWriteFactory, name: &str) -> bool {
    unsafe {
        let mut coll = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(coll) = coll else { return false };
        let n = crate::util::wide(name);
        let mut idx = 0u32;
        let mut exists = windows::core::BOOL(0);
        let _ = coll.FindFamilyName(PCWSTR(n.as_ptr()), &mut idx, &mut exists);
        exists.as_bool()
    }
}

/// Drawing API used by the island. All units are DIPs.
pub struct Canvas<'a> {
    pub g: &'a mut Gfx,
    rt: ID2D1DCRenderTarget,
}

impl Canvas<'_> {
    fn brush(&self, c: Color) -> &ID2D1SolidColorBrush {
        let b = self.g.brush.as_ref().unwrap();
        unsafe { b.SetColor(&c) };
        b
    }

    pub fn translate(&self, x: f32, y: f32) {
        unsafe { self.rt.SetTransform(&Matrix3x2::translation(x, y)) };
    }

    pub fn fill_rect(&self, r: RectF, radius: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.brush(c)) };
    }

    pub fn stroke_rect(&self, r: RectF, radius: f32, c: Color, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.brush(c), width, None) };
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, self.brush(c)) };
    }

    pub fn fill_ellipse(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rx, radiusY: ry };
        unsafe { self.rt.FillEllipse(&e, self.brush(c)) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, w: f32) {
        unsafe { self.rt.DrawLine(pt(x0, y0), pt(x1, y1), self.brush(c), w, None) };
    }

    pub fn radial(&self, cx: f32, cy: f32, r: f32, stops: &[(f32, Color)]) {
        self.radial_offset(cx, cy, r, 0.0, 0.0, stops);
    }

    /// Circle at (cx, cy) whose gradient origin is shifted by (ox, oy), for
    /// a lit-from-the-side look.
    pub fn radial_offset(&self, cx: f32, cy: f32, r: f32, ox: f32, oy: f32, stops: &[(f32, Color)]) {
        unsafe {
            let s: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: *c }).collect();
            let Ok(coll) = self.rt.CreateGradientStopCollection(&s, D2D1_GAMMA_1_0, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: pt(cx, cy),
                gradientOriginOffset: pt(ox, oy),
                radiusX: r,
                radiusY: r,
            };
            if let Ok(b) = self.rt.CreateRadialGradientBrush(&props, None, &coll) {
                let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
                self.rt.FillEllipse(&e, &b);
            }
        }
    }

    pub fn fill_geometry_vgradient(&self, geo: &ID2D1Geometry, top: f32, bottom: f32, c0: Color, c1: Color) {
        unsafe {
            let s = [D2D1_GRADIENT_STOP { position: 0.0, color: c0 }, D2D1_GRADIENT_STOP { position: 1.0, color: c1 }];
            let Ok(coll) = self.rt.CreateGradientStopCollection(&s, D2D1_GAMMA_1_0, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(0.0, top), endPoint: pt(0.0, bottom) };
            if let Ok(b) = self.rt.CreateLinearGradientBrush(&props, None, &coll) {
                self.rt.FillGeometry(geo, &b, None);
            }
        }
    }

    pub fn hgradient_rect(&self, r: RectF, radius: f32, stops: &[(f32, Color)]) {
        unsafe {
            let s: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: *c }).collect();
            let Ok(coll) = self.rt.CreateGradientStopCollection(&s, D2D1_GAMMA_1_0, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(r.x, 0.0), endPoint: pt(r.x + r.w, 0.0) };
            if let Ok(b) = self.rt.CreateLinearGradientBrush(&props, None, &coll) {
                let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
                self.rt.FillRoundedRectangle(&rr, &b);
            }
        }
    }

    pub fn stroke_geometry(&self, geo: &ID2D1Geometry, c: Color, w: f32) {
        unsafe { self.rt.DrawGeometry(geo, self.brush(c), w, None) };
    }

    pub fn push_clip(&self, geo: &ID2D1Geometry) {
        unsafe {
            let params = D2D1_LAYER_PARAMETERS {
                contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
                geometricMask: std::mem::ManuallyDrop::new(Some(geo.clone())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            self.rt.PushLayer(&params, None);
            // PushLayer AddRefs what it needs; release our copy.
            std::mem::ManuallyDrop::into_inner(params.geometricMask);
        }
    }

    pub fn push_clip_rect(&self, r: RectF) {
        unsafe { self.rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_clip_rect(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt.PopLayer() };
    }

    /// Single-line or wrapped text inside `r`, vertically centred.
    pub fn text(&mut self, s: &str, r: RectF, font: Font, size: f32, weight: i32, align: Align, c: Color) {
        self.text_ex(s, r, font, size, weight, align, c, false);
    }

    pub fn text_wrapped(&mut self, s: &str, r: RectF, font: Font, size: f32, weight: i32, align: Align, c: Color) {
        self.text_ex(s, r, font, size, weight, align, c, true);
    }

    fn text_ex(&mut self, s: &str, r: RectF, font: Font, size: f32, weight: i32, align: Align, c: Color, wrap: bool) {
        let l = self.g.layout(s, font, size, weight, align, wrap, r.w, r.h);
        unsafe { self.rt.DrawTextLayout(pt(r.x, r.y), &l, self.brush(c), D2D1_DRAW_TEXT_OPTIONS_NONE) };
    }

    pub fn draw_layout(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: Color) {
        unsafe { self.rt.DrawTextLayout(pt(x, y), l, self.brush(c), D2D1_DRAW_TEXT_OPTIONS_NONE) };
    }

    pub fn icon(&mut self, glyph: char, cx: f32, cy: f32, size: f32, c: Color) {
        let r = RectF::new(cx - size, cy - size, size * 2.0, size * 2.0);
        self.text(&glyph.to_string(), r, Font::Icons, size, 400, Align::Center, c);
    }
}

/// The notch: flush with the top of the screen, flaring into the edge with
/// concave "ramps", rounded at the bottom. Units are DIPs; `cx` is the centre.
pub fn notch_geometry(f: &ID2D1Factory, cx: f32, w: f32, d: f32, r: f32, rw: f32, rh: f32) -> Result<ID2D1PathGeometry> {
    unsafe {
        let geo = f.CreatePathGeometry()?;
        let sink = geo.Open()?;
        let l = cx - w / 2.0;
        let rt = cx + w / 2.0;
        let rh = rh.min(d * 0.45).max(0.0);
        let r = r.min(d - rh).min(w / 2.0).max(0.0);
        sink.BeginFigure(pt(l - rw, -2.0), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(pt(l - rw, 0.0));
        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: pt(l - rw * 0.42, 0.0), point2: pt(l, rh * 0.58), point3: pt(l, rh) });
        sink.AddLine(pt(l, d - r));
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: pt(l + r, d),
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(pt(rt - r, d));
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: pt(rt, d - r),
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(pt(rt, rh));
        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: pt(rt, rh * 0.58), point2: pt(rt + rw * 0.42, 0.0), point3: pt(rt + rw, 0.0) });
        sink.AddLine(pt(rt + rw, -2.0));
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geo)
    }
}
