// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Key layout definitions in abstract grid units; rendering scales them to pixels.

/// What a key does when pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Insert a character (shifted variant used while shift is latched).
    Char {
        base: char,
        shifted: char,
    },
    /// Insert a fixed character regardless of shift state.
    Fixed(char),
    Space,
    Backspace,
    Submit,
    Cancel,
    /// Latch/unshift shift for the current layer, keeping cursor position.
    Shift,
    /// Switch to a different layer index.
    Layer(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// Position and size in grid units (column, row, width, height).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub name: &'static str,
    pub cols: f32,
    pub rows: f32,
    pub keys: Vec<Key>,
}

impl Layer {
    pub fn find(&self, x: f32, y: f32) -> Option<usize> {
        self.keys
            .iter()
            .position(|k| x >= k.x && x < k.x + k.w && y >= k.y && y < k.y + k.h)
    }
}

/// Build the default layered layout.
///
/// Layers: 0 = alphanumeric, 1 = symbols, 2 = numpad/navigation-ish.
pub fn default_layout() -> Vec<Layer> {
    vec![alpha_layer(), symbols_layer(), numpad_layer()]
}

fn control_row(out: &mut Vec<Key>, cols: f32, y: f32) {
    out.push(Key {
        x: 0.0,
        y,
        w: 1.5,
        h: 1.0,
        action: Action::Layer(1),
    });
    out.push(Key {
        x: 1.5,
        y,
        w: cols - 1.5 - 1.5 - 1.5,
        h: 1.0,
        action: Action::Space,
    });
    out.push(Key {
        x: cols - 3.0,
        y,
        w: 1.5,
        h: 1.0,
        action: Action::Cancel,
    });
    out.push(Key {
        x: cols - 1.5,
        y,
        w: 1.5,
        h: 1.0,
        action: Action::Submit,
    });
}

fn row(chars: &[&str], y: f32, indent: f32) -> Vec<Key> {
    chars
        .iter()
        .enumerate()
        .map(|(i, def)| {
            let base = def.chars().next().unwrap();
            let shifted = def.chars().nth(1).unwrap_or(base);
            Key {
                x: indent + i as f32,
                y,
                w: 1.0,
                h: 1.0,
                action: Action::Char { base, shifted },
            }
        })
        .collect()
}

fn alpha_layer() -> Layer {
    let cols = 10.0f32;
    let mut keys = Vec::new();
    keys.extend(row(
        &["qQ", "wW", "eE", "rR", "tT", "yY", "uU", "iI", "oO", "pP"],
        0.0,
        0.0,
    ));
    keys.extend(row(
        &["aA", "sS", "dD", "fF", "gG", "hH", "jJ", "kK", "lL"],
        1.0,
        0.5,
    ));
    keys.push(Key {
        x: 0.0,
        y: 2.0,
        w: 1.5,
        h: 1.0,
        action: Action::Shift,
    });
    keys.extend(row(&["zZ", "xX", "cC", "vV", "bB", "nN", "mM"], 2.0, 1.5));
    keys.push(Key {
        x: 8.5,
        y: 2.0,
        w: 1.5,
        h: 1.0,
        action: Action::Backspace,
    });
    control_row(&mut keys, cols, 3.0);
    Layer {
        name: "abc",
        cols,
        rows: 4.0,
        keys,
    }
}

fn symbols_layer() -> Layer {
    let cols = 10.0f32;
    let mut keys = Vec::new();
    keys.extend(row(
        &["1!", "2@", "3#", "4$", "5%", "6^", "7&", "8*", "9(", "0)"],
        0.0,
        0.0,
    ));
    keys.extend(row(
        &["-_", "[{", "]}", "{|", ";:", "'\"", ",<", ".>", "/?"],
        1.0,
        0.0,
    ));
    keys.push(Key {
        x: 0.0,
        y: 2.0,
        w: 1.5,
        h: 1.0,
        action: Action::Layer(0),
    });
    keys.extend(row(&["`~", "\\|", "<+", ">=", "#≠", "$€", "*‡"], 2.0, 1.5));
    keys.push(Key {
        x: 8.5,
        y: 2.0,
        w: 1.5,
        h: 1.0,
        action: Action::Backspace,
    });
    control_row(&mut keys, cols, 3.0);
    Layer {
        name: "?123",
        cols,
        rows: 4.0,
        keys,
    }
}

fn numpad_layer() -> Layer {
    let cols = 6.0f32;
    let mut keys = Vec::new();
    let grid = [["7&", "8*", "9("], ["4$", "5%", "6^"], ["1!", "2@", "3#"]];
    for (r, line) in grid.iter().enumerate() {
        keys.extend(row(line, r as f32, 1.0));
    }
    keys.extend(row(&["0)", ".·", ",;"], 3.0, 1.0));
    for r in 0..4 {
        keys.push(Key {
            x: 4.0,
            y: r as f32,
            w: 2.0,
            h: 1.0,
            action: if r == 3 {
                Action::Submit
            } else {
                Action::Backspace
            },
        });
    }
    // First column: layer access + cancel stacked via control-style keys.
    keys.push(Key {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
        action: Action::Layer(0),
    });
    keys.push(Key {
        x: 0.0,
        y: 1.0,
        w: 1.0,
        h: 1.0,
        action: Action::Layer(1),
    });
    keys.push(Key {
        x: 0.0,
        y: 2.0,
        w: 1.0,
        h: 1.0,
        action: Action::Layer(2),
    });
    keys.push(Key {
        x: 0.0,
        y: 3.0,
        w: 1.0,
        h: 1.0,
        action: Action::Cancel,
    });
    Layer {
        name: "num",
        cols,
        rows: 4.0,
        keys,
    }
}
