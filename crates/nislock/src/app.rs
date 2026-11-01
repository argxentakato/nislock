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

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use calloop::channel::{self, Channel};
use calloop::signals::{Signal, Signals};
use calloop::{EventLoop, LoopHandle};
use nislock_input::spawn_deck_reader;
use nislock_kbd::{Event as KbdEvent, Keyboard};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry, delegate_seat,
    delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
    seat::{Capability, SeatHandler, SeatState},
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    backend::ObjectId,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_keyboard::WlKeyboard, wl_output, wl_seat, wl_surface},
};
use xkeysym::key;

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
    seat_state: SeatState,
    registry_state: RegistryState,
    shm: Shm,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    loop_handle: LoopHandle<'static, App>,
    /// Physical keyboards, keyed by seat object id; one per seat that offers
    /// the keyboard capability.
    keyboards: HashMap<ObjectId, SeatKeyboard>,
    views: Views,
    keyboard: Keyboard,
    model: LockModel,
    controller: LockController,
    dirty: bool,
    exit: Option<i32>,
}

/// A physical keyboard from one seat: the protocol object, plus which of our
/// lock surfaces the compositor has given keyboard focus to.
struct SeatKeyboard {
    keyboard: WlKeyboard,
    focused: Option<wl_surface::WlSurface>,
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
///
/// The event loop lifetime is `'static` because the auto-repeat timer for
/// physical keyboards holds a loop handle that lives as long as the app.
pub fn bootstrap(config: &Config) -> Result<Runtime<'static>, NisError> {
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
    // Physical keyboards come from seats; the registry handler below also
    // picks up seats/keyboards plugged in after startup.
    let seat_state = SeatState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);
    let shm = Shm::bind(&globals, &qh).map_err(NisError::NoShm)?;
    let session_lock_state = SessionLockState::new(&globals, &qh);

    // Deck gamepad: hidraw reader thread -> std mpsc -> calloop channel.
    // Opened before requesting the lock so a missing pad fails cleanly, not as a crashed locker.
    let (pad_tx, pad_rx) = mpsc::channel();
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

    let loop_handle = event_loop.handle();
    let mut app = App {
        conn: conn.clone(),
        compositor_state,
        output_state,
        seat_state,
        registry_state,
        shm,
        session_lock_state,
        session_lock: None,
        loop_handle,
        keyboards: HashMap::new(),
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

    fn seat_keyboard(&self, keyboard: &WlKeyboard) -> Option<&SeatKeyboard> {
        let id = keyboard.id();
        self.keyboards.values().find(|sk| sk.keyboard.id() == id)
    }

    fn seat_keyboard_mut(&mut self, keyboard: &WlKeyboard) -> Option<&mut SeatKeyboard> {
        let id = keyboard.id();
        self.keyboards
            .values_mut()
            .find(|sk| sk.keyboard.id() == id)
    }

    /// Feed one physical key event (a press, or an auto-repeat from the
    /// repeat timer) through the widget events for typing/editing keys.
    fn handle_physical_key(&mut self, event: &KeyEvent, repeat: bool) {
        for ev in physical_key_events(event, repeat) {
            self.handle_kbd_event(ev);
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

fn bridge_gamepad_events(rx: mpsc::Receiver<KbdEvent>, cal_tx: channel::Sender<KbdEvent>) {
    while let Ok(ev) = rx.recv() {
        if cal_tx.send(ev).is_err() {
            break;
        }
    }
}

/// Translate one physical key event into zero or more widget events.
/// Backspace/Delete edit the password field, Return submits, Escape clears;
/// any other printable text becomes direct input. Auto-repeat keeps typing
/// and deleting but never re-fires submit or cancel, so holding a key
/// cannot re-run the auth policy repeatedly. Navigation, shift, and layer
/// events are deliberately never produced: whatever is typed on a physical
/// keyboard leaves the on-screen cursor position, shift state, and layer
/// exactly where the gamepad left them.
fn physical_key_events(event: &KeyEvent, repeat: bool) -> Vec<KbdEvent> {
    match event.keysym.raw() {
        key::BackSpace | key::Delete => return vec![KbdEvent::Backspace],
        key::Return | key::KP_Enter if !repeat => return vec![KbdEvent::Submit],
        key::Escape if !repeat => return vec![KbdEvent::Cancel],
        _ => {}
    }
    event
        .utf8
        .iter()
        .flat_map(|text| text.chars())
        .filter(|c| *c == ' ' || !c.is_control())
        .map(KbdEvent::TextInput)
        .collect()
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

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Keyboard || self.keyboards.contains_key(&seat.id()) {
            return;
        }
        // A keyboard failure never fails the lock itself: the gamepad path
        // stays available, and a hot-plugged keyboard is picked up again on
        // the next capability announcement.
        let loop_handle = self.loop_handle.clone();
        let repeat_callback = Box::new(|app: &mut App, _keyboard: &WlKeyboard, event: KeyEvent| {
            app.handle_physical_key(&event, true);
        });
        match self.seat_state.get_keyboard_with_repeat(
            qh,
            &seat,
            None,
            loop_handle,
            repeat_callback,
        ) {
            Ok(keyboard) => {
                self.keyboards.insert(
                    seat.id(),
                    SeatKeyboard {
                        keyboard,
                        focused: None,
                    },
                );
            }
            Err(e) => eprintln!("nislock: keyboard on seat unavailable: {e}"),
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            self.keyboards.remove(&seat.id());
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.keyboards.remove(&seat.id());
    }
}

impl KeyboardHandler for App {
    #[allow(clippy::too_many_arguments)]
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if let Some(sk) = self.seat_keyboard_mut(keyboard) {
            sk.focused = Some(surface.clone());
        }
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if let Some(sk) = self.seat_keyboard_mut(keyboard) {
            sk.focused = None;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        // Only keys typed while one of our lock surfaces has focus reach the
        // auth path.
        let focused = self
            .seat_keyboard(keyboard)
            .and_then(|sk| sk.focused.clone());
        if focused.is_some_and(|surface| self.views.has_surface(&surface)) {
            self.handle_physical_key(&event, false);
        }
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _layout: u32,
    ) {
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState,];
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(App);
delegate_keyboard!(App);
delegate_output!(App);
delegate_registry!(App);
delegate_seat!(App);
delegate_session_lock!(App);
delegate_shm!(App);
wayland_client::delegate_noop!(App: ignore wl_buffer::WlBuffer);

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(keysym: u32, utf8: Option<&str>) -> KeyEvent {
        KeyEvent {
            time: 0,
            raw_code: 0,
            keysym: Keysym::new(keysym),
            utf8: utf8.map(str::to_string),
        }
    }

    fn typed(keysym: u32, text: &str) -> Vec<KbdEvent> {
        physical_key_events(&key_event(keysym, Some(text)), false)
    }

    #[test]
    fn printable_text_becomes_text_input_events() {
        assert_eq!(typed(key::a, "a"), [KbdEvent::TextInput('a')]);
        assert_eq!(typed(key::A, "A"), [KbdEvent::TextInput('A')]);
        assert_eq!(typed(key::exclam, "!"), [KbdEvent::TextInput('!')]);
        assert_eq!(typed(key::space, " "), [KbdEvent::TextInput(' ')]);
        // Compose sequences can produce multi-byte text: keep every char.
        assert_eq!(typed(key::ae, "æ"), [KbdEvent::TextInput('æ')]);
    }

    #[test]
    fn control_characters_and_empty_text_produce_nothing() {
        // A repeated Enter still carries the original "\r" utf8: control
        // characters never reach the password field.
        assert!(physical_key_events(&key_event(key::Return, Some("\r")), true).is_empty());
        // Non-printable keysyms arrive with empty or None utf8.
        assert!(physical_key_events(&key_event(key::NoSymbol, Some("")), false).is_empty());
        assert!(physical_key_events(&key_event(key::F1, None), false).is_empty());
    }

    #[test]
    fn editing_and_action_keysyms_map_to_widget_events() {
        assert_eq!(
            physical_key_events(&key_event(key::BackSpace, None), false),
            [KbdEvent::Backspace]
        );
        assert_eq!(
            physical_key_events(&key_event(key::Delete, None), false),
            [KbdEvent::Backspace]
        );
        assert_eq!(
            physical_key_events(&key_event(key::Return, Some("\r")), false),
            [KbdEvent::Submit]
        );
        assert_eq!(
            physical_key_events(&key_event(key::KP_Enter, Some("\r")), false),
            [KbdEvent::Submit]
        );
        assert_eq!(
            physical_key_events(&key_event(key::Escape, None), false),
            [KbdEvent::Cancel]
        );
    }

    #[test]
    fn auto_repeat_types_and_deletes_but_never_submits_or_cancels() {
        assert_eq!(
            physical_key_events(&key_event(key::BackSpace, None), true),
            [KbdEvent::Backspace]
        );
        assert_eq!(
            physical_key_events(&key_event(key::a, Some("a")), true),
            [KbdEvent::TextInput('a')]
        );
        assert!(physical_key_events(&key_event(key::Return, Some("\r")), true).is_empty());
        assert!(physical_key_events(&key_event(key::Escape, None), true).is_empty());
    }
}
