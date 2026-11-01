// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Painting: draws the keyboard into an XRGB8888 frame using tiny-skia + fontdue.
//! Software rendering only, so the same code can later target wl_shm, DRM dumb
//! buffers, or an initrd framebuffer without GPU access.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fontdue::{Font, FontSettings};
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    Transform,
};

use crate::error::KbdError;
use crate::{Action, Keyboard};

const FONT_BYTES: &[u8] = include_bytes!("../assets/IBMPlexSans-Regular.ttf");

/// XRGB8888 destination frame (byte order B,G,R,x on little-endian).
pub struct Frame<'a> {
    pub pixels: &'a mut [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Idle,
    Busy,
    Error,
    Info,
}

/// Text content owned by the backend (locker, greeter, initrd prompt...).
pub struct Screen<'a> {
    pub title: &'a str,
    pub user: &'a str,
    pub status: &'a str,
    pub status_kind: StatusKind,
    pub hint: &'a str,
}

struct Glyph {
    pixmap: Pixmap,
    left: i32,
    top: i32,
    advance: f32,
}

mod theme {
    use tiny_skia::Color;
    // from_rgba8 is not const in tiny-skia 0.11, so these are inline fns.
    pub fn bg() -> Color {
        Color::from_rgba8(16, 18, 27, 255)
    }
    pub fn key() -> Color {
        Color::from_rgba8(45, 50, 66, 255)
    }
    pub fn key_alt() -> Color {
        Color::from_rgba8(36, 40, 54, 255)
    }
    pub fn key_focus() -> Color {
        Color::from_rgba8(122, 162, 247, 255)
    }
    pub fn key_active() -> Color {
        Color::from_rgba8(63, 72, 96, 255)
    }
    pub fn label() -> Color {
        Color::from_rgba8(198, 204, 220, 255)
    }
    pub fn label_dim() -> Color {
        Color::from_rgba8(126, 134, 154, 255)
    }
    pub fn field() -> Color {
        Color::from_rgba8(24, 26, 38, 255)
    }
    pub fn error() -> Color {
        Color::from_rgba8(247, 118, 142, 255)
    }
    pub fn info() -> Color {
        Color::from_rgba8(176, 183, 200, 255)
    }
    pub fn accent() -> Color {
        Color::from_rgba8(122, 162, 247, 255)
    }
    pub fn white() -> Color {
        Color::from_rgba8(255, 255, 255, 255)
    }
}

fn color_rgb(c: Color) -> u32 {
    let r = (c.red() * 255.0) as u32;
    let g = (c.green() * 255.0) as u32;
    let b = (c.blue() * 255.0) as u32;
    (r << 16) | (g << 8) | b
}

pub struct Renderer {
    font: Font,
    // Cache key: (char, pixel size, tint rgb) — only a few tints occur per screen.
    cache: RefCell<HashMap<(char, u32, u32), Rc<Glyph>>>,
}

impl Renderer {
    /// Build with the embedded default font.
    pub fn embedded() -> Self {
        Self::from_bytes(FONT_BYTES).expect("embedded font must load")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KbdError> {
        let font =
            Font::from_bytes(bytes, FontSettings::default()).map_err(|e| KbdError::FontLoad {
                message: e.to_string(),
            })?;
        Ok(Self {
            font,
            cache: RefCell::new(HashMap::new()),
        })
    }

    fn glyph(&self, c: char, size: u32, tint: Color) -> Rc<Glyph> {
        let rgb = color_rgb(tint);
        if let Some(g) = self.cache.borrow().get(&(c, size, rgb)) {
            return Rc::clone(g);
        }
        let (metrics, bitmap) = self.font.rasterize(c, size as f32);
        let (w, h) = (metrics.width, metrics.height);
        let mut pm = Pixmap::new(w.max(1) as u32, h.max(1) as u32).expect("1x1 pixmap");
        if w > 0 && h > 0 {
            let tr = (tint.red() * 255.0) as u32;
            let tg = (tint.green() * 255.0) as u32;
            let tb = (tint.blue() * 255.0) as u32;
            // tiny-skia pixmap bytes are premultiplied RGBA (R,G,B,A)
            let px = pm.data_mut();
            for (i, cov) in bitmap.iter().enumerate() {
                let a = *cov as u32;
                let o = i * 4;
                px[o] = ((tr * a) / 255) as u8;
                px[o + 1] = ((tg * a) / 255) as u8;
                px[o + 2] = ((tb * a) / 255) as u8;
                px[o + 3] = *cov;
            }
        }
        let g = Rc::new(Glyph {
            pixmap: pm,
            left: metrics.xmin,
            // top edge above the baseline in font-up coords
            top: metrics.ymin + h as i32,
            advance: metrics.advance_width,
        });
        self.cache
            .borrow_mut()
            .insert((c, size, rgb), Rc::clone(&g));
        g
    }

    fn text_width(&self, s: &str, size: u32) -> f32 {
        s.chars()
            .map(|c| self.glyph(c, size, theme::white()).advance)
            .sum()
    }

    fn draw_text(&self, pm: &mut Pixmap, s: &str, size: u32, x: f32, baseline: f32, color: Color) {
        let size = size.clamp(6, 512);
        let paint = PixmapPaint {
            opacity: 1.0,
            blend_mode: BlendMode::SourceOver,
            quality: FilterQuality::Nearest,
        };
        let mut pen = x.round() as i32;
        for c in s.chars() {
            let g = self.glyph(c, size, color);
            if g.pixmap.width() > 0 && g.pixmap.height() > 0 {
                pm.draw_pixmap(
                    pen + g.left,
                    baseline.round() as i32 - g.top,
                    g.pixmap.as_ref(),
                    &paint,
                    Transform::default(),
                    None,
                );
            }
            pen += g.advance.round() as i32;
        }
    }

    fn fill_round_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, fill: Color) {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let mut pb = PathBuilder::new();
        let k = 0.5523;
        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.cubic_to(x + w - r + r * k, y, x + w, y + r - r * k, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.cubic_to(
            x + w,
            y + h - r + r * k,
            x + w - r + r * k,
            y + h,
            x + w - r,
            y + h,
        );
        pb.line_to(x + r, y + h);
        pb.cubic_to(x + r - r * k, y + h, x, y + h - r + r * k, x, y + h - r);
        pb.line_to(x, y + r);
        pb.cubic_to(x, y + r - r * k, x + r - r * k, y, x + r, y);
        pb.close();
        let Some(path) = pb.finish() else { return };
        Self::fill_path(pm, &path, fill);
    }

    fn fill_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, fill: Color) {
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, r);
        let Some(path) = pb.finish() else { return };
        Self::fill_path(pm, &path, fill);
    }

    fn fill_path(pm: &mut Pixmap, path: &Path, fill: Color) {
        let mut paint = Paint::default();
        paint.set_color(fill);
        paint.anti_alias = true;
        pm.fill_path(path, &paint, FillRule::Winding, Transform::default(), None);
    }

    /// Paint the whole screen (top band + keyboard) into `frame`.
    pub fn draw(&self, kb: &mut Keyboard, frame: &mut Frame, screen: &Screen) {
        kb.layout_pass(frame.width, frame.height);
        let Some(mut pm) = Pixmap::new(frame.width, frame.height) else {
            return;
        };
        pm.fill(theme::bg());

        let w = frame.width as f32;
        let h = frame.height as f32;
        let pad = w.min(h) * 0.03;
        let title_size = (h * 0.045) as u32;
        let body_size = (h * 0.03) as u32;

        // --- top band -------------------------------------------------------
        self.draw_text(
            &mut pm,
            screen.title,
            title_size,
            pad,
            pad + title_size as f32,
            theme::label(),
        );
        let user_line = format!("user: {}", screen.user);
        self.draw_text(
            &mut pm,
            &user_line,
            body_size,
            pad,
            pad + title_size as f32 * 1.9,
            theme::label_dim(),
        );

        let dots = kb.buffer_text().chars().count();
        let scene_field = kb.scene().field;
        if let Some((fx, fy, fw, fh)) = scene_field {
            Self::fill_round_rect(&mut pm, fx, fy, fw, fh, fh * 0.25, theme::field());
            // draw bullet dots as circles: the embedded font may lack U+25CF
            let dot_r = fh * 0.14;
            let step = fh * 0.55;
            let cy = fy + fh / 2.0;
            let mut caret_x = fx + fh * 0.5 + dot_r;
            for _ in 0..dots {
                Self::fill_circle(&mut pm, caret_x, cy, dot_r, theme::accent());
                caret_x += step;
            }
            Self::fill_round_rect(
                &mut pm,
                caret_x + 4.0,
                fy + fh * 0.25,
                2.0,
                fh * 0.5,
                1.0,
                theme::accent(),
            );
        }

        let status_color = match screen.status_kind {
            StatusKind::Error => theme::error(),
            StatusKind::Info | StatusKind::Busy => theme::info(),
            StatusKind::Idle => theme::label_dim(),
        };
        if !screen.status.is_empty() {
            self.draw_text(
                &mut pm,
                screen.status,
                body_size,
                pad,
                pad + title_size as f32 * 1.9 + body_size as f32 * 1.5,
                status_color,
            );
        }
        if !screen.hint.is_empty() {
            self.draw_text(
                &mut pm,
                screen.hint,
                (body_size * 3 / 4).max(10),
                pad,
                h - pad,
                theme::label_dim(),
            );
        }

        // --- keyboard -------------------------------------------------------
        let unit = kb.scene().unit;
        let radius = unit * 0.12;
        for (i, k) in kb.scene().keys.iter().enumerate() {
            let focused = i == kb.focus_index();
            let base = match k.action {
                Action::Submit => theme::key_active(),
                Action::Cancel | Action::Backspace | Action::Layer(_) | Action::Shift => {
                    theme::key_alt()
                }
                _ => theme::key(),
            };
            let fill = if focused { theme::key_focus() } else { base };
            Self::fill_round_rect(
                &mut pm,
                k.x + 1.0,
                k.y + 1.0,
                k.w - 2.0,
                k.h - 2.0,
                radius,
                fill,
            );
            let label_color = if focused { theme::bg() } else { theme::label() };
            let label_size = ((k.h.min(k.w)) * 0.3) as u32;
            let tw = self.text_width(&k.label, label_size);
            self.draw_text(
                &mut pm,
                &k.label,
                label_size,
                (k.x + k.w / 2.0 - tw / 2.0).max(k.x + 2.0),
                k.y + k.h / 2.0 + label_size as f32 * 0.35,
                label_color,
            );
        }

        // --- blit: tiny-skia stores premultiplied RGBA; wl_shm XRGB needs B,G,R,x
        let src = pm.data();
        for y in 0..frame.height as usize {
            let srow = y * frame.width as usize * 4;
            let drow = y * frame.stride as usize;
            for x in 0..frame.width as usize {
                let s = srow + x * 4;
                let d = drow + x * 4;
                frame.pixels[d] = src[s + 2];
                frame.pixels[d + 1] = src[s + 1];
                frame.pixels[d + 2] = src[s];
                frame.pixels[d + 3] = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dir, Event, Keyboard};

    #[test]
    fn draw_writes_nonempty_frame_and_uses_stride() {
        let r = Renderer::embedded();
        let mut kb = Keyboard::new();
        for c in "ab1".chars() {
            kb.handle(Event::TextInput(c));
        }
        kb.handle(Event::Nav(Dir::Right));
        let mut buf1 = vec![0u8; 640 * 400 * 4];
        let mut buf2 = vec![0u8; (660 * 400) * 4]; // wider stride than width
        {
            let mut frame = Frame {
                pixels: &mut buf1,
                width: 640,
                height: 400,
                stride: 640 * 4,
            };
            let screen = Screen {
                title: "nislock",
                user: "deck",
                status: "wrong password",
                status_kind: StatusKind::Error,
                hint: "A: select B: delete",
            };
            r.draw(&mut kb, &mut frame, &screen);
        }
        {
            let mut frame = Frame {
                pixels: &mut buf2,
                width: 640,
                height: 400,
                stride: 660 * 4,
            };
            let screen = Screen {
                title: "nislock",
                user: "deck",
                status: "",
                status_kind: StatusKind::Idle,
                hint: "",
            };
            r.draw(&mut kb, &mut frame, &screen);
        }
        // some non-background pixel must have been painted
        assert!(buf1.chunks(4).any(|p| p != [0, 0, 0, 0]));
        // with a padded stride the trailing padding must stay untouched
        for y in 0..400usize {
            let pad = &buf2[y * 660 * 4 + 640 * 4..y * 660 * 4 + 660 * 4];
            assert!(pad.iter().all(|&b| b == 0), "stride padding was written");
        }
        // XRGB little-endian: channels must be B,G,R order — verify against the
        // known background color (16,18,27)
        assert_eq!(&buf1[..4], &[27, 18, 16, 0]);
    }

    #[test]
    fn draw_survives_arbitrary_output_sizes() {
        let r = Renderer::embedded();
        let screen = Screen {
            title: "nislock",
            user: "deck",
            status: "wrong password",
            status_kind: StatusKind::Error,
            hint: "A: select",
        };
        for (w, h) in [
            (1, 1),
            (7, 3),
            (320, 200),
            (1280, 800),
            (1876, 1098),
            (3840, 2160),
            (800, 1280),
            (4000, 60),
        ] {
            let mut kb = Keyboard::new();
            let mut buf = vec![0u8; w * h * 4];
            let mut frame = Frame {
                pixels: &mut buf,
                width: w as u32,
                height: h as u32,
                stride: (w * 4) as u32,
            };
            r.draw(&mut kb, &mut frame, &screen);
        }
    }
}
