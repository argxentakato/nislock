// SPDX-License-Identifier: AGPL-3.0-only
// Copyright 2026 nislock contributors
//! Renders the lock screen to a PNG without a Wayland display.
//!
//! `cargo run -p nislock-kbd --example snapshot -- out.png`
//! The PNG encoder is deliberately minimal (stored deflate blocks) so the
//! example needs no extra dependencies.

use nislock_kbd::Dir;
use nislock_kbd::Event;
use nislock_kbd::Frame;
use nislock_kbd::Keyboard;
use nislock_kbd::Renderer;
use nislock_kbd::Screen;
use nislock_kbd::StatusKind;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "nislock-snapshot.png".to_string());
    let (w, h) = (1280usize, 800usize);

    let mut kb = Keyboard::new();
    for c in "hunter2".chars() {
        kb.handle(Event::TextInput(c));
    }
    for _ in 0..12 {
        kb.handle(Event::Nav(Dir::Right));
    }
    kb.handle(Event::Nav(Dir::Down));

    let mut px = vec![0u8; w * h * 4];
    {
        let mut frame = Frame {
            pixels: &mut px,
            width: w as u32,
            height: h as u32,
            stride: (w * 4) as u32,
        };
        let screen = Screen {
            title: "nislock",
            user: "deck",
            status: "",
            status_kind: StatusKind::Idle,
            hint: "A: select  B: delete  START: submit  L1/R1: layer",
        };
        Renderer::embedded().draw(&mut kb, &mut frame, &screen);
    }

    write_png(&path, w, h, &px);
    println!("wrote {path}");

    if std::env::args().any(|a| a == "--dump-scene") {
        for k in kb.scene().keys.iter() {
            println!(
                "scene {:.1},{:.1} {:.1}x{:.1} {:?} label={:?}",
                k.x, k.y, k.w, k.h, k.action, k.label
            );
        }
    }
}

fn write_png(path: &str, w: usize, h: usize, bgrx: &[u8]) {
    // filter-0 scanlines, RGB from BGRx
    let mut raw = Vec::with_capacity(h * (w * 3 + 1));
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let i = (y * w + x) * 4;
            // frame is B,G,R,x little-endian; PNG wants R,G,B
            raw.extend_from_slice(&[bgrx[i + 2], bgrx[i + 1], bgrx[i]]);
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    chunk(&mut png, b"IHDR", &[
        &(w as u32).to_be_bytes(),
        &(h as u32).to_be_bytes(),
        &[8, 2, 0, 0, 0],
    ]);

    // zlib stream with uncompressed deflate blocks
    let mut deflate = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        deflate.push(u8::from(i + 1 == blocks.len()));
        deflate.extend_from_slice(&(block.len() as u16).to_le_bytes());
        deflate.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        deflate.extend_from_slice(block);
    }
    deflate.extend_from_slice(&adler32(&raw).to_be_bytes());
    chunk(&mut png, b"IDAT", &[&deflate]);
    chunk(&mut png, b"IEND", &[]);

    std::fs::write(path, png).expect("write png");
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], parts: &[&[u8]]) {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(kind);
    let mut body = Vec::with_capacity(len + 4);
    for p in parts {
        body.extend_from_slice(p);
    }
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &d in data {
        a = (a + d as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}
