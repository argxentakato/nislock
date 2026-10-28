// SPDX-License-Identifier: AGPL-3.0-only
// Copyright 2026 nislock contributors
//! nislock — ext-session-lock-v1 locker driven by the Steam Deck gamepad.
//!
//! The compositor hands us one lock surface per output via
//! ext-session-lock-v1; we paint the shared on-screen keyboard (`nislock-kbd`)
//! into wl_shm buffers and consume the Deck gamepad directly from the
//! hid-steam control-interface hidraw (`nislock-input`) so there is no
//! keyboard/modifier (lizard mode) interference through the compositor.

use std::collections::HashMap;
use std::time::Duration;

use calloop::EventLoop;
use calloop::channel::{self, Channel};
use nislock_kbd::{Effect, Event as KbdEvent, Keyboard, Renderer, Screen, StatusKind};
use nislock_input::spawn_deck_reader;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_registry, delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    Proxy,
    backend::ObjectId,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use zeroize::Zeroizing;

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
    current: Option<smithay_client_toolkit::shm::slot::Buffer>,
}

struct ScreenTexts {
    user: String,
    status: String,
    status_kind: StatusKind,
}

struct App {
    conn: Connection,
    compositor_state: CompositorState,
    output_state: OutputState,
    registry_state: RegistryState,
    shm: Shm,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    /// Surfaces created at lock/hot-plug time, waiting for their first
    /// configure event (which carries the size we need for shm buffers).
    pending_locks: HashMap<ObjectId, SessionLockSurface>,
    lock_surfaces: HashMap<ObjectId, LockSurface>,
    keyboard: Keyboard,
    renderer: Renderer,
    screen: ScreenTexts,
    dirty: bool,
    exit: Option<i32>,
}

impl App {
    fn handle_kbd_event(&mut self, ev: KbdEvent) {
        match self.keyboard.handle(ev) {
            Effect::Noop => {}
            Effect::Redraw => self.dirty = true,
            // Exiting while locked without unlock makes the compositor report a crash.
            Effect::Cancel => {
                self.keyboard.clear();
                self.dirty = true;
            }
            Effect::Submit(password) => {
                self.dirty = true;
                if verify_password(&self.screen.user, &password) {
                    self.screen.status = "welcome!".into();
                    self.screen.status_kind = StatusKind::Info;
                    self.unlock_and_exit(0);
                } else {
                    self.screen.status = "wrong password".into();
                    self.screen.status_kind = StatusKind::Error;
                    self.keyboard.clear();
                }
            }
        }
    }

    /// Send unlock (when still locked), flush it, then terminate.
    fn unlock_and_exit(&mut self, code: i32) {
        if let Some(lock) = self.session_lock.take()
            && lock.is_locked()
        {
            lock.unlock();
        }
        // The spec requires a display round-trip before the client exits so the
        // compositor has processed unlock_and_destroy.
        let _ = self.conn.roundtrip();
        self.exit = Some(code);
    }

    fn draw_all(&mut self) {
        let ids: Vec<_> = self.lock_surfaces.keys().cloned().collect();
        for id in ids {
            let Some(mut ls) = self.lock_surfaces.remove(&id) else { continue };
            if !ls.surface.wl_surface().is_alive() {
                // The compositor destroyed this surface; dropping the pool
                // munmaps its memfd regions so hot-plug cycles do not leak.
                continue;
            }
            self.draw_surface(&mut ls);
            self.lock_surfaces.insert(id, ls);
        }
    }

    fn draw_surface(&mut self, ls: &mut LockSurface) {
        let stride = ls.width as usize * 4;
        let mut painted = false;
        if let Some(buf) = ls.current.as_ref()
            && let Some(canvas) = ls.pool.canvas(buf)
        {
            self.paint(canvas, ls.width, ls.height, stride);
            painted = true;
        }
        if !painted {
            match ls.pool.create_buffer(
                ls.width as i32,
                ls.height as i32,
                stride as i32,
                wl_shm::Format::Xrgb8888,
            ) {
                Ok((buf, canvas)) => {
                    self.paint(canvas, ls.width, ls.height, stride);
                    ls.current = Some(buf);
                    painted = true;
                }
                Err(e) => eprintln!("nislock: shm create_buffer failed: {e}"),
            }
        }
        if painted
            && let Some(buf) = ls.current.as_ref()
        {
            let wl = ls.surface.wl_surface();
            if let Err(e) = buf.attach_to(wl) {
                eprintln!("nislock: attach failed: {e}");
                return;
            }
            wl.damage(0, 0, ls.width as i32, ls.height as i32);
            wl.commit();
        }
    }

    fn paint(&mut self, pixels: &mut [u8], width: u32, height: u32, stride: usize) {
        let mut frame = nislock_kbd::Frame { pixels, width, height, stride: stride as u32 };
        let screen = Screen {
            title: TITLE,
            user: &self.screen.user,
            status: &self.screen.status,
            status_kind: self.screen.status_kind,
            hint: HINT,
        };
        self.renderer.draw(&mut self.keyboard, &mut frame, &screen);
    }
}

fn main() -> std::process::ExitCode {
    let user = std::env::var("NISLOCK_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "deck".into());

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nislock: cannot connect to Wayland compositor: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (globals, event_queue) = match registry_queue_init(&conn) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("nislock: registry init failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let qh: QueueHandle<App> = event_queue.handle();
    let Ok(mut event_loop) = EventLoop::<App>::try_new() else {
        eprintln!("nislock: cannot create event loop");
        return std::process::ExitCode::FAILURE;
    };

    // Like swaylock/hyprlock, SIGUSR1 unlocks and exits. signalfd needs the
    // signal masked before any thread is spawned.
    {
        use calloop::signals::{Signal, Signals};
        let Ok(signals) = Signals::new(&[Signal::SIGUSR1]) else {
            eprintln!("nislock: cannot create signal source");
            return std::process::ExitCode::FAILURE;
        };
        if event_loop
            .handle()
            .insert_source(signals, |_, _, app| {
                eprintln!("nislock: SIGUSR1 received; unlocking");
                app.unlock_and_exit(0);
            })
            .is_err()
        {
            eprintln!("nislock: cannot register signal source");
            return std::process::ExitCode::FAILURE;
        }
    }

    let Ok(compositor_state) = CompositorState::bind(&globals, &qh) else {
        eprintln!("nislock: no wl_compositor");
        return std::process::ExitCode::FAILURE;
    };
    let output_state = OutputState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);
    let Ok(shm) = Shm::bind(&globals, &qh) else {
        eprintln!("nislock: no wl_shm");
        return std::process::ExitCode::FAILURE;
    };
    let session_lock_state = SessionLockState::new(&globals, &qh);

    // Deck gamepad: hidraw reader thread -> std mpsc -> calloop channel.
    // Opened before requesting the lock so a missing pad fails cleanly, not as a crashed locker.
    let (pad_tx, pad_rx) = std::sync::mpsc::channel();
    let _pad = match spawn_deck_reader(pad_tx) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("nislock: gamepad detection failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (cal_tx, cal_rx): (channel::Sender<KbdEvent>, Channel<KbdEvent>) = channel::channel();
    std::thread::spawn(|| bridge_gamepad_events(pad_rx, cal_tx));
    if event_loop
        .handle()
        .insert_source(cal_rx, |item, _, app| match item {
            channel::Event::Msg(ev) => app.handle_kbd_event(ev),
            channel::Event::Closed => eprintln!("nislock: gamepad input closed; staying locked"),
        })
        .is_err()
    {
        eprintln!("nislock: cannot register gamepad source");
        return std::process::ExitCode::FAILURE;
    }

    let mut app = App {
        conn: conn.clone(),
        compositor_state,
        output_state,
        registry_state,
        shm,
        session_lock_state,
        session_lock: None,
        pending_locks: HashMap::new(),
        lock_surfaces: HashMap::new(),
        keyboard: Keyboard::new(),
        renderer: Renderer::embedded(),
        screen: ScreenTexts { user, status: String::new(), status_kind: StatusKind::Idle },
        dirty: false,
        exit: None,
    };

    app.session_lock = match app.session_lock_state.lock(&qh) {
        Ok(lock) => Some(lock),
        Err(e) => {
            eprintln!("nislock: ext-session-lock-v1 unavailable: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if WaylandSource::new(conn, event_queue).insert(event_loop.handle()).is_err() {
        eprintln!("nislock: cannot register wayland source");
        return std::process::ExitCode::FAILURE;
    }

    while app.exit.is_none() {
        if let Err(e) = event_loop.dispatch(Duration::from_millis(50), &mut app) {
            eprintln!("nislock: event loop error: {e}");
            break;
        }
        if app.dirty && !app.lock_surfaces.is_empty() {
            app.draw_all();
            app.dirty = false;
        }
    }

    let code = app.exit.unwrap_or(1);
    if code != 0 {
        eprintln!("nislock: lock ended with code {code}");
    }
    std::process::ExitCode::from(code as u8)
}

fn bridge_gamepad_events(rx: std::sync::mpsc::Receiver<KbdEvent>, cal_tx: channel::Sender<KbdEvent>) {
    while let Ok(ev) = rx.recv() {
        if cal_tx.send(ev).is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// authentication
// ---------------------------------------------------------------------------

fn verify_password(user: &str, password: &Zeroizing<String>) -> bool {
    let service = std::env::var("NISLOCK_PAM_SERVICE").unwrap_or_else(|_| "login".into());
    match pam::Authenticator::with_password(&service) {
        Ok(mut auth) => {
            auth.get_handler().set_credentials(user, password.as_str());
            auth.authenticate().is_ok()
        }
        Err(e) => {
            eprintln!("nislock: cannot open PAM service {service}: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// wayland glue
// ---------------------------------------------------------------------------

impl SessionLockHandler for App {
    fn locked(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, lock: SessionLock) {
        for output in self.output_state.outputs() {
            let surface = self.compositor_state.create_surface(qh);
            let lock_surface = lock.create_lock_surface(surface, &output, qh);
            self.pending_locks.insert(lock_surface.wl_surface().id(), lock_surface);
        }
    }

    fn finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        // The compositor ended the lock (external unlock, or a failed lock).
        self.exit.get_or_insert(1);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let (width, height) = configure.new_size;
        let id = surface.wl_surface().id();
        // Configured now; drop the waiting placeholder so the map does not
        // hold destroyed surfaces across lock cycles.
        self.pending_locks.remove(&id);
        let pool_len = width as usize * height as usize * 4;
        let existing = self.lock_surfaces.get_mut(&id);
        if let Some(ls) = existing {
            ls.width = width;
            ls.height = height;
            ls.current = None;
            ls.pool = match SlotPool::new(pool_len, &self.shm) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("nislock: cannot re-allocate shm pool: {e}");
                    self.lock_surfaces.remove(&id);
                    self.dirty = false;
                    return;
                }
            };
        } else {
            match SlotPool::new(pool_len, &self.shm) {
                Ok(pool) => {
                    self.lock_surfaces
                        .insert(id, LockSurface { surface, width, height, pool, current: None });
                }
                Err(e) => eprintln!("nislock: cannot allocate shm pool: {e}"),
            }
        }
        self.dirty = true;
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // Hot-plug while locked: give the new output a lock surface too.
        if let Some(lock) = self.session_lock.as_ref().filter(|l| l.is_locked()) {
            let surface = self.compositor_state.create_surface(qh);
            let lock_surface = lock.create_lock_surface(surface, &output, qh);
            self.pending_locks.insert(lock_surface.wl_surface().id(), lock_surface);
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        self.dirty = true;
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.pending_locks.remove(&output.id());
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState,];
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_registry!(App);
delegate_session_lock!(App);
delegate_shm!(App);
wayland_client::delegate_noop!(App: ignore wl_buffer::WlBuffer);
