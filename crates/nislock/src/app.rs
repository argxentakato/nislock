// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Application shell + framework adapters.
//!
//! This module owns the smithay-client-toolkit state (Wayland globals, the
//! session-lock protocol object, shm) and the handler implementations the
//! delegate macros require. Those impls are deliberately thin adapters: the
//! protocol events are translated into calls on the Controller (policy), the
//! View (surfaces/painting), and the Model (screen state). The Wayland
//! object bookkeeping itself lives in [`crate::view`].

use std::time::Duration;

use calloop::EventLoop;
use calloop::channel::{self, Channel};
use calloop::signals::{Signal, Signals};
use nislock_input::spawn_deck_reader;
use nislock_kbd::{Event as KbdEvent, Keyboard};
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
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_surface},
};

use crate::auth::PamAuthenticator;
use crate::config::Config;
use crate::controller::{ControlEffect, LockController};
use crate::error::NisError;
use crate::model::LockModel;
use crate::view::Views;

pub struct App {
    conn: Connection,
    compositor_state: CompositorState,
    output_state: OutputState,
    registry_state: RegistryState,
    shm: Shm,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    views: Views,
    keyboard: Keyboard,
    model: LockModel,
    controller: LockController,
    dirty: bool,
    exit: Option<i32>,
}

/// The event loop and its data live together; `run` destructures so the
/// dispatch call can keep them as disjoint borrows.
pub struct Runtime<'a> {
    event_loop: EventLoop<'a, App>,
    app: App,
}

impl Runtime<'_> {
    pub fn run(&mut self) -> Result<i32, NisError> {
        let Runtime { event_loop, app } = self;
        while app.exit.is_none() {
            if let Err(e) = event_loop.dispatch(Duration::from_millis(50), app) {
                eprintln!("nislock: event loop error: {e}");
                break;
            }
            if app.dirty && app.views.has_configured_surfaces() {
                app.views.draw_all(&mut app.keyboard, &app.model);
                app.dirty = false;
            }
        }
        let code = app.exit.take().unwrap_or(1);
        if code != 0 {
            eprintln!("nislock: lock ended with code {code}");
        }
        Ok(code)
    }
}

/// Connect, bind the globals we need, register every event source, request
/// the session lock, and hand back the ready-to-run pair.
pub fn bootstrap(config: &Config) -> Result<Runtime<'_>, NisError> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh: QueueHandle<App> = event_queue.handle();
    let event_loop = EventLoop::<App>::try_new().map_err(NisError::EventLoopStart)?;

    // Like swaylock/hyprlock, SIGUSR1 unlocks and exits. signalfd needs the
    // signal masked before any thread is spawned.
    let signals = Signals::new(&[Signal::SIGUSR1]).map_err(NisError::SignalSource)?;
    event_loop
        .handle()
        .insert_source(signals, |_, _, app| {
            eprintln!("nislock: SIGUSR1 received; unlocking");
            app.unlock_and_exit(0);
        })
        .map_err(|e| NisError::SignalRegistration(e.error))?;

    let compositor_state = CompositorState::bind(&globals, &qh).map_err(NisError::NoCompositor)?;
    let output_state = OutputState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);
    let shm = Shm::bind(&globals, &qh).map_err(NisError::NoShm)?;
    let session_lock_state = SessionLockState::new(&globals, &qh);

    // Deck gamepad: hidraw reader thread -> std mpsc -> calloop channel.
    // Opened before requesting the lock so a missing pad fails cleanly, not as a crashed locker.
    let (pad_tx, pad_rx) = std::sync::mpsc::channel();
    let _pad = spawn_deck_reader(pad_tx)?;
    let (cal_tx, cal_rx): (channel::Sender<KbdEvent>, Channel<KbdEvent>) = channel::channel();
    std::thread::spawn(move || bridge_gamepad_events(pad_rx, cal_tx));
    event_loop
        .handle()
        .insert_source(cal_rx, |item, _, app| match item {
            channel::Event::Msg(ev) => app.handle_kbd_event(ev),
            channel::Event::Closed => eprintln!("nislock: gamepad input closed; staying locked"),
        })
        .map_err(|e| NisError::GamepadRegistration(e.error))?;

    let mut app = App {
        conn: conn.clone(),
        compositor_state,
        output_state,
        registry_state,
        shm,
        session_lock_state,
        session_lock: None,
        views: Views::new(),
        keyboard: Keyboard::new(),
        model: LockModel::new(config.user.clone()),
        controller: LockController::new(Box::new(PamAuthenticator::new())),
        dirty: false,
        exit: None,
    };

    app.session_lock = Some(
        app.session_lock_state
            .lock(&qh)
            .map_err(NisError::SessionLockUnavailable)?,
    );

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| NisError::WaylandRegistration(e.error))?;

    Ok(Runtime { event_loop, app })
}

impl App {
    fn handle_kbd_event(&mut self, ev: KbdEvent) {
        match self
            .controller
            .handle_event(ev, &mut self.keyboard, &mut self.model)
        {
            ControlEffect::None => {}
            ControlEffect::Redraw => self.dirty = true,
            ControlEffect::UnlockExit(code) => {
                self.dirty = true;
                self.unlock_and_exit(code);
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
}

fn bridge_gamepad_events(
    rx: std::sync::mpsc::Receiver<KbdEvent>,
    cal_tx: channel::Sender<KbdEvent>,
) {
    while let Ok(ev) = rx.recv() {
        if cal_tx.send(ev).is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// wayland glue (framework adapters: protocol events -> controller/view/model)
// ---------------------------------------------------------------------------

impl SessionLockHandler for App {
    fn locked(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, lock: SessionLock) {
        for output in self.output_state.outputs() {
            let surface = self.compositor_state.create_surface(qh);
            let lock_surface = lock.create_lock_surface(surface, &output, qh);
            self.views.track_pending(lock_surface);
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
        self.dirty = self
            .views
            .surface_configured(&self.shm, surface, width, height);
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
            self.views.track_pending(lock_surface);
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
        self.views.clear_pending(&output.id());
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
