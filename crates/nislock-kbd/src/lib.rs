//! Backend-agnostic on-screen keyboard: layout, input state machine, and painting.
//!
//! The widget only knows abstract [`Event`]s and abstract [`Effect`]s so the same
//! code can drive a Wayland locker, an SDDM overlay, or an initrd framebuffer.

mod layout;
mod render;

pub use layout::{Action, Key, Layer, default_layout};

/// A point in layout coordinate space (grid units before scaling).
#[derive(Copy, Clone, Debug, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

pub use render::{Frame, Renderer, Screen, StatusKind};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// Controller/keyboard/touch input normalized into widget actions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    Nav(Dir),
    /// Activate the focused key (A button / Enter on a key).
    Press,
    /// Direct backspace shortcut (B button), independent of focus.
    Backspace,
    ToggleShift,
    NextLayer,
    PrevLayer,
    /// Direct submit shortcut (Start button).
    Submit,
    /// Direct cancel shortcut (Select button).
    Cancel,
    /// Absolute pointer position, normalized 0..1 within the frame.
    Pointer { x: f32, y: f32 },
    /// Click/tap at the last pointer position.
    Click,
    /// A character typed on a physical keyboard, appended verbatim.
    TextInput(char),
}

#[derive(Debug)]
pub enum Effect {
    Noop,
    Redraw,
    /// The user requested unlock with this password.
    Submit(Zeroizing<String>),
    /// The user requested cancel/unlock without authenticating.
    Cancel,
}

/// A key resolved to concrete pixel geometry for the current layout pass.
#[derive(Clone, Debug)]
pub struct PaintedKey {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
    pub action: Action,
}

/// Geometry computed by the last `layout_pass`; used by draw and hit-testing.
/// Exposed for backends that need to hit-test or overlay the scene.
#[derive(Default)]
pub struct Scene {
    pub keys: Vec<PaintedKey>,
    pub field: Option<(f32, f32, f32, f32)>,
    pub unit: f32,
}

pub struct Keyboard {
    layers: Vec<Layer>,
    layer: usize,
    shift: bool,
    buffer: Zeroizing<String>,
    focus: usize,
    pointer: Option<(f32, f32)>,
    scene: Scene,
    last_size: (u32, u32),
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    pub fn new() -> Self {
        let layers = default_layout();
        Self {
            layers,
            layer: 0,
            shift: false,
            buffer: Zeroizing::new(String::new()),
            focus: 0,
            pointer: None,
            scene: Scene::default(),
            last_size: (0, 0),
        }
    }

    /// Feed a normalized input event; returns an effect the backend must act on.
    pub fn handle(&mut self, ev: Event) -> Effect {
        match ev {
            Event::Nav(dir) => {
                self.nearest_focus(dir);
                Effect::Redraw
            }
            Event::Press => self.press_focused(),
            Event::Backspace => {
                self.pop();
                Effect::Redraw
            }
            Event::ToggleShift => {
                self.shift = !self.shift;
                Effect::Redraw
            }
            Event::NextLayer => self.set_layer(self.layer + 1),
            Event::PrevLayer => {
                let n = self.layers.len();
                self.set_layer((self.layer + n - 1) % n)
            }
            Event::Submit => self.submit(),
            Event::Cancel => Effect::Cancel,
            Event::Pointer { x, y } => {
                self.pointer = Some((x, y));
                self.focus_at_normalized(x, y)
            }
            Event::Click => {
                if self.pointer.is_some() {
                    self.press_focused()
                } else {
                    Effect::Noop
                }
            }
            Event::TextInput(c) => {
                if c == '\u{8}' {
                    self.pop();
                } else if c == '\r' || c == '\n' {
                    return self.submit();
                } else if c == ' ' || !c.is_control() {
                    self.buffer.push(c);
                } else {
                    return Effect::Noop;
                }
                Effect::Redraw
            }
        }
    }

    pub fn shift_latched(&self) -> bool {
        self.shift
    }

    pub fn active_layer(&self) -> usize {
        self.layer
    }

    pub fn focus_index(&self) -> usize {
        self.focus
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.shift = false;
        self.focus = 0;
    }

    fn set_layer(&mut self, idx: usize) -> Effect {
        let n = self.layers.len();
        self.layer = idx % n;
        self.focus = self.focus.min(self.layers[self.layer].keys.len() - 1);
        Effect::Redraw
    }

    fn pop(&mut self) {
        self.buffer.pop();
    }

    fn submit(&mut self) -> Effect {
        if self.buffer.is_empty() {
            return Effect::Noop;
        }
        let pw = std::mem::replace(&mut self.buffer, Zeroizing::new(String::new()));
        Effect::Submit(pw)
    }

    fn char_for(&self, action: Action) -> Option<char> {
        match action {
            Action::Char { base, shifted } => {
                Some(if self.shift { shifted } else { base })
            }
            Action::Fixed(c) => Some(c),
            Action::Space => Some(' '),
            _ => None,
        }
    }

    fn press_focused(&mut self) -> Effect {
        let Some(key) = self.layers[self.layer].keys.get(self.focus).copied() else {
            return Effect::Noop;
        };
        self.pressed(key.action)
    }

    fn pressed(&mut self, action: Action) -> Effect {
        if let Some(c) = self.char_for(action) {
            self.buffer.push(c);
            if self.shift {
                self.shift = false;
            }
            return Effect::Redraw;
        }
        match action {
            Action::Shift => {
                self.shift = !self.shift;
                Effect::Redraw
            }
            Action::Backspace => {
                self.pop();
                Effect::Redraw
            }
            Action::Submit => self.submit(),
            Action::Cancel => Effect::Cancel,
            Action::Layer(i) => self.set_layer(i),
            _ => Effect::Noop,
        }
    }

    fn focus_at_normalized(&mut self, nx: f32, ny: f32) -> Effect {
        let (w, h) = self.last_size;
        if w == 0 || h == 0 {
            return Effect::Noop;
        }
        let px = nx * w as f32;
        let py = ny * h as f32;
        match self
            .scene
            .keys
            .iter()
            .position(|k| px >= k.x && px < k.x + k.w && py >= k.y && py < k.y + k.h)
        {
            Some(i) => {
                self.focus = i;
                Effect::Redraw
            }
            None => Effect::Noop,
        }
    }

    fn nearest_focus(&mut self, dir: Dir) {
        let keys = &self.scene.keys;
        if keys.is_empty() {
            return;
        }
        let idx = self.focus;
        let cur = &keys[idx];
        let (cx, cy) = (cur.x + cur.w / 2.0, cur.y + cur.h / 2.0);
        let (cw, ch) = (cur.w, cur.h);
        let unit = self.scene.unit.max(1.0);
        let mut best: Option<(f32, usize)> = None;
        for (i, k) in keys.iter().enumerate() {
            if i == idx {
                continue;
            }
            let (kx, ky) = (k.x + k.w / 2.0, k.y + k.h / 2.0);
            let (dx, dy) = (kx - cx, ky - cy);
            let (along, cross) = match dir {
                Dir::Right => (dx, dy),
                Dir::Left => (-dx, dy),
                Dir::Down => (dy, dx),
                Dir::Up => (-dy, dx),
            };
            if along <= unit * 0.2 {
                continue;
            }
            let same_lane = (dy.abs() < ch * 0.75 && matches!(dir, Dir::Left | Dir::Right))
                || (dx.abs() < cw * 0.6 && matches!(dir, Dir::Up | Dir::Down));
            let score = along
                + if same_lane {
                    0.0
                } else {
                    cross.abs() * 1.6 + along * 0.15
                };
            if best.is_none_or(|(s, _)| score < s) {
                best = Some((score, i));
            }
        }
        if let Some((_, i)) = best {
            self.focus = i;
        }
    }

    /// Compute pixel geometry for the given frame size; cached until size changes.
    pub(crate) fn layout_pass(&mut self, width: u32, height: u32) {
        if self.last_size == (width, height) && !self.scene.keys.is_empty() {
            return;
        }
        let w = width as f32;
        let h = height as f32;
        let pad = w.min(h) * 0.03;
        // Top band: caption + password field. Bottom: keyboard fitted preserving aspect.
        let top_h = h * 0.28;
        let field = (pad * 2.0, top_h * 0.52, w - pad * 4.0, top_h * 0.3);
        let area_x = pad;
        let area_y = top_h + pad * 0.5;
        let area_w = w - pad * 2.0;
        let area_h = h - area_y - pad;
        let layer = &self.layers[self.layer];
        let scale = (area_w / layer.cols).min(area_h / layer.rows);
        let kb_w = layer.cols * scale;
        let kb_h = layer.rows * scale;
        let kx = area_x + (area_w - kb_w) / 2.0;
        let ky = area_y + (area_h - kb_h) / 2.0;
        let keys: Vec<PaintedKey> = layer
            .keys
            .iter()
            .map(|k| PaintedKey {
                x: kx + k.x * scale,
                y: ky + k.y * scale,
                w: k.w * scale,
                h: k.h * scale,
                label: label_for(k, self.shift),
                action: k.action,
            })
            .collect();
        self.focus = self.focus.min(keys.len().saturating_sub(1));
        self.scene = Scene { keys, field: Some(field), unit: scale };
        self.last_size = (width, height);
    }

    /// Current pixel geometry of the last layout pass (empty until first draw
    /// or an explicit frontend resize notification).
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub(crate) fn buffer_text(&self) -> &str {
        &self.buffer
    }
}

fn label_for(key: &Key, shift: bool) -> String {
    match key.action {
        Action::Char { base, shifted } => {
            if shifted != base {
                format!("{base} {shifted}")
            } else {
                base.to_string().to_uppercase()
            }
        }
        Action::Fixed(c) => c.to_string(),
        Action::Space => "space".into(),
        Action::Backspace => "del".into(),
        Action::Submit => "unlock".into(),
        Action::Cancel => "cancel".into(),
        Action::Shift => {
            if shift { "shift ON".into() } else { "shift".into() }
        }
        Action::Layer(_) => "sym".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid_out() -> Keyboard {
        let mut kb = Keyboard::new();
        kb.layout_pass(1280, 800);
        kb
    }

    #[test]
    fn scene_is_built_and_inside_bounds() {
        let kb = laid_out();
        assert!(!kb.scene.keys.is_empty());
        for k in &kb.scene.keys {
            assert!(k.x >= -0.01 && k.y >= -0.01, "key off-screen: {k:?}");
            assert!(k.x + k.w <= 1280.01 && k.y + k.h <= 800.01, "key clipped: {k:?}");
        }
        assert!(kb.scene.field.is_some());
    }

    #[test]
    fn typing_then_submit_yields_and_clears_password() {
        let mut kb = Keyboard::new();
        for c in "abc".chars() {
            assert!(matches!(kb.handle(Event::TextInput(c)), Effect::Redraw));
        }
        match kb.handle(Event::Submit) {
            Effect::Submit(pw) => assert_eq!(&*pw, "abc"),
            other => panic!("expected Submit, got {other:?}"),
        }
        assert!(kb.buffer_text().is_empty());
        assert!(matches!(kb.handle(Event::Submit), Effect::Noop));
    }

    #[test]
    fn backspace_edits_buffer() {
        let mut kb = Keyboard::new();
        kb.handle(Event::TextInput('a'));
        kb.handle(Event::TextInput('b'));
        kb.handle(Event::Backspace);
        assert_eq!(kb.buffer_text(), "a");
        kb.handle(Event::TextInput('\u{8}'));
        assert!(kb.buffer_text().is_empty());
    }

    #[test]
    fn pressing_focused_char_key_types_it() {
        let mut kb = laid_out();
        // focus starts at key 0 of the first layer
        let f = kb.focus_index();
        let act = kb.layers[0].keys[f].action;
        let expect = match act {
            Action::Char { base, .. } => base,
            Action::Fixed(c) => c,
            _ => panic!("expected first key to be a character key, got {act:?}"),
        };
        assert!(matches!(kb.handle(Event::Press), Effect::Redraw));
        assert_eq!(kb.buffer_text(), expect.to_string());
    }

    #[test]
    fn shift_latch_applies_to_next_press_only() {
        let mut kb = laid_out();
        let f = kb.focus_index();
        let (base, shifted) = match kb.layers[0].keys[f].action {
            Action::Char { base, shifted } => (base, shifted),
            act => panic!("expected char key at focus, got {act:?}"),
        };
        kb.handle(Event::ToggleShift);
        assert!(kb.shift_latched());
        kb.handle(Event::Press);
        assert_eq!(kb.buffer_text(), shifted.to_string());
        assert!(!kb.shift_latched());
        kb.handle(Event::Press);
        assert_eq!(kb.buffer_text(), format!("{}{}", shifted, base));
    }

    #[test]
    fn nav_moves_focus_to_a_key_in_that_direction() {
        let mut kb = laid_out();
        let f0 = kb.focus_index();
        let x0 = kb.scene.keys[f0].x;
        kb.handle(Event::Nav(Dir::Right));
        let f1 = kb.focus_index();
        assert_ne!(f1, f0);
        assert!(kb.scene.keys[f1].x > x0);

        let (x1, y1) = (kb.scene.keys[f1].x, kb.scene.keys[f1].y);
        kb.handle(Event::Nav(Dir::Down));
        let f2 = kb.focus_index();
        assert_ne!(f2, f1);
        assert!(kb.scene.keys[f2].y > y1);

        kb.handle(Event::Nav(Dir::Left));
        let f3 = kb.focus_index();
        assert!(kb.scene.keys[f3].x < x1);
    }

    #[test]
    fn layer_switch_wraps_around() {
        let mut kb = Keyboard::new();
        let n = kb.layers.len();
        for i in 1..=n + 1 {
            kb.handle(Event::NextLayer);
            assert_eq!(kb.active_layer(), i % n);
        }
        kb.handle(Event::PrevLayer);
        assert_eq!(kb.active_layer(), 0);
    }

    #[test]
    fn pointer_click_focuses_then_press_activates() {
        let mut kb = laid_out();
        // pick an arbitrary key in the middle of the scene and click its center
        let idx = kb.scene.keys.len() / 2;
        let k = kb.scene.keys[idx].clone();
        let nx = (k.x + k.w / 2.0) / 1280.0;
        let ny = (k.y + k.h / 2.0) / 800.0;
        assert!(matches!(kb.handle(Event::Pointer { x: nx, y: ny }), Effect::Redraw));
        assert_eq!(kb.focus_index(), idx);
        let before = kb.buffer_text().to_string();
        kb.handle(Event::Click);
        if let Action::Char { base, .. } = k.action {
            assert_eq!(kb.buffer_text(), format!("{before}{base}"));
        }
    }
}
