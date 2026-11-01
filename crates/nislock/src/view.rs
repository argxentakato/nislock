// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! View: one ext-session-lock surface per output, painted via `nislock-kbd`
//! into wl_shm buffers.
//!
//! The View owns the per-output surface/bookkeeping state (`pending_locks`
//! waiting for their first configure, configured `lock_surfaces` with their
//! shm pools) and knows how to render a `LockModel` + `Keyboard` onto them.
//! It never decides *what* the state means; the Controller/Model do.

use std::collections::HashMap;

use nislock_kbd::{Frame, Keyboard, Renderer, Screen};
use smithay_client_toolkit::session_lock::SessionLockSurface;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_client::backend::ObjectId;
use wayland_client::{
    Proxy,
    protocol::{wl_shm, wl_surface},
};

use crate::model::LockModel;

/// Caption shown on the lock screen.
const TITLE: &str = "nislock";
const HINT: &str = "A: select   B: delete   START: submit   L1/R1: layer   SELECT: layer";

struct LockSurface {
    surface: SessionLockSurface,
    width: u32,
    height: u32,
    pool: SlotPool,
    /// Last buffer handed to the compositor; SlotPool gives us its canvas
    /// back once the compositor releases it (double-buffered otherwise a new
    /// buffer is created).
    current: Option<Buffer>,
}

pub struct Views {
    /// Surfaces created at lock/hot-plug time, waiting for their first
    /// configure event (which carries the size we need for shm buffers).
    pending_locks: HashMap<ObjectId, SessionLockSurface>,
    lock_surfaces: HashMap<ObjectId, LockSurface>,
    renderer: Renderer,
}

impl Views {
    pub fn new() -> Self {
        Self {
            pending_locks: HashMap::new(),
            lock_surfaces: HashMap::new(),
            renderer: Renderer::embedded(),
        }
    }

    /// Track a freshly created lock surface until it is configured.
    pub fn track_pending(&mut self, lock_surface: SessionLockSurface) {
        self.pending_locks
            .insert(lock_surface.wl_surface().id(), lock_surface);
    }

    /// Drop a pending placeholder (configured, or its output went away).
    pub fn clear_pending(&mut self, id: &ObjectId) {
        self.pending_locks.remove(id);
    }

    /// True once at least one surface has a configured size to draw into.
    pub fn has_configured_surfaces(&self) -> bool {
        !self.lock_surfaces.is_empty()
    }

    /// Whether this Wayland surface is one of our lock surfaces, pending or
    /// configured. The keyboard handlers use this to accept physical key
    /// input only when one of our surfaces has keyboard focus.
    pub fn has_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        let id = surface.id();
        self.pending_locks.contains_key(&id) || self.lock_surfaces.contains_key(&id)
    }

    /// A surface was (re-)configured with a size: (re-)allocate its shm pool.
    /// The return value is the new value for the shell's redraw flag.
    pub fn surface_configured(
        &mut self,
        shm: &Shm,
        surface: SessionLockSurface,
        width: u32,
        height: u32,
    ) -> bool {
        let id = surface.wl_surface().id();
        // Configured now; drop the waiting placeholder so the map does not
        // hold destroyed surfaces across lock cycles.
        self.clear_pending(&id);
        let pool_len = width as usize * height as usize * 4;
        let existing = self.lock_surfaces.get_mut(&id);
        if let Some(ls) = existing {
            ls.width = width;
            ls.height = height;
            ls.current = None;
            ls.pool = match SlotPool::new(pool_len, shm) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("nislock: cannot re-allocate shm pool: {e}");
                    self.lock_surfaces.remove(&id);
                    return false;
                }
            };
        } else {
            match SlotPool::new(pool_len, shm) {
                Ok(pool) => {
                    self.lock_surfaces.insert(
                        id,
                        LockSurface {
                            surface,
                            width,
                            height,
                            pool,
                            current: None,
                        },
                    );
                }
                Err(e) => eprintln!("nislock: cannot allocate shm pool: {e}"),
            }
        }
        true
    }

    /// Repaint every live surface; dead surfaces are dropped (with their
    /// pools) so hot-plug cycles do not leak memfd mappings.
    pub fn draw_all(&mut self, keyboard: &mut Keyboard, model: &LockModel) {
        let Self {
            lock_surfaces,
            renderer,
            ..
        } = self;
        lock_surfaces.retain(|_, ls| {
            let alive = ls.surface.wl_surface().is_alive();
            if alive {
                draw_surface(renderer, ls, keyboard, model);
            }
            alive
        });
    }
}

impl Default for Views {
    fn default() -> Self {
        Self::new()
    }
}

fn draw_surface(
    renderer: &Renderer,
    ls: &mut LockSurface,
    keyboard: &mut Keyboard,
    model: &LockModel,
) {
    let (width, height) = (ls.width, ls.height);
    let stride = width as usize * 4;

    // Repaint the previous buffer if the compositor has released it;
    // otherwise allocate a fresh one.
    let repainted = match ls.current.as_ref().and_then(|buf| ls.pool.canvas(buf)) {
        Some(canvas) => {
            paint(renderer, canvas, width, height, stride, keyboard, model);
            true
        }
        None => false,
    };
    if !repainted {
        match ls.pool.create_buffer(
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Xrgb8888,
        ) {
            Ok((buf, canvas)) => {
                paint(renderer, canvas, width, height, stride, keyboard, model);
                ls.current = Some(buf);
            }
            Err(e) => {
                eprintln!("nislock: shm create_buffer failed: {e}");
                return;
            }
        }
    }

    let Some(buf) = ls.current.as_ref() else {
        return;
    };
    let wl = ls.surface.wl_surface();
    if let Err(e) = buf.attach_to(wl) {
        eprintln!("nislock: attach failed: {e}");
        return;
    }
    wl.damage(0, 0, width as i32, height as i32);
    wl.commit();
}

fn paint(
    renderer: &Renderer,
    pixels: &mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    keyboard: &mut Keyboard,
    model: &LockModel,
) {
    let mut frame = Frame {
        pixels,
        width,
        height,
        stride: stride as u32,
    };
    let screen = Screen {
        title: TITLE,
        user: &model.user,
        status: &model.status,
        status_kind: model.status_kind,
        hint: HINT,
    };
    renderer.draw(keyboard, &mut frame, &screen);
}
