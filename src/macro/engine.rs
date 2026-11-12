use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use handy_sdk::input::key::{ENTER, ESC, Key, KeyKind, MOUSE_LEFT, R, SHIFT, SPACE, key_from_label, resolve_key};
use handy_sdk::input::send::{press, release, tap};
use handy_sdk::util::timing::precise_wait;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, SetEvent, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL, WaitForSingleObject,
};

use super::decisions::{
    MacroView, Op, Plan, counter_step, on_x1_down, on_x1_up, on_x2_up, reset_plan, to_view, x2_ground_step,
};
use super::input_rules::{EventType, InputEvent, InputQueue};
use crate::app::config::Settings;
use crate::game::shiftlock::ShiftlockTracker;
use crate::game::state::{Footing, GameState};

static HELD_JUMPSET: AtomicU64 = AtomicU64::new(0); // pack_key(jumpset_held), read by the crash handler

/// Key <-> one atomic word (0 = none), so the crash path can read the held key without a lock.
pub fn pack_key(k: &Key) -> u64 {
    1u64 << 48
        | u64::from(k.kind == KeyKind::Mouse) << 40
        | u64::from(k.extended) << 32
        | u64::from(k.mouse_button as u8) << 16
        | u64::from(k.scan)
}

pub fn unpack_key(v: u64) -> Option<Key> {
    if v == 0 {
        return None;
    }
    Some(Key {
        kind: if v >> 40 & 1 != 0 {
            KeyKind::Mouse
        } else {
            KeyKind::Keyboard
        },
        scan: v as u16,
        extended: v >> 32 & 1 != 0,
        mouse_button: i32::from((v >> 16) as u8),
    })
}

/// The jumpset key an Engine holds: published on press, cleared on release/abort. Any thread.
pub fn held_jumpset_key() -> Option<Key> {
    unpack_key(HELD_JUMPSET.load(Relaxed))
}

/// For on_crash / on_end_session (a plain fn): releases held_jumpset_key(), if any, and every other key the engine
/// sends. The ultimate and spike keys are configurable with no lock-free mirror, so their defaults (LCTRL, M1) are
/// released. A key-up for a key that isn't down does nothing.
pub fn release_held_keys() {
    if let Some(k) = held_jumpset_key() {
        release(&k);
    }
    for k in [SHIFT, SPACE, ESC, R, ENTER, MOUSE_LEFT] {
        release(&k);
    }
    if let Some(k) = key_from_label("LCTRL") {
        release(&k);
    }
}

/// An auto-reset Win32 event, closed on drop: the hooks set it, the engine waits on it.
pub struct WakeEvent(HANDLE);

// SAFETY: an event handle may be set and waited on from any thread.
unsafe impl Send for WakeEvent {}
unsafe impl Sync for WakeEvent {}

impl WakeEvent {
    /// None when CreateEventW fails.
    pub fn new() -> Option<Self> {
        unsafe { CreateEventW(None, false, false, None) }.ok().map(Self)
    }
    pub fn set(&self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
    fn wait(&self, ms: u32) {
        unsafe { WaitForSingleObject(self.0, ms) };
    }
}

impl Drop for WakeEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The value Engine::shiftlock() reads; any thread.
pub fn shiftlock_of(v: &AtomicI8) -> Option<bool> {
    let v = v.load(Relaxed);
    if v < 0 { None } else { Some(v == 1) }
}

/// Macro thread: consumes hook events, steps the shiftlock tracker, executes Plans as injected input (press/tap).
pub struct Engine {
    queue: Arc<InputQueue>,
    wake: Arc<WakeEvent>,
    sample: Box<dyn FnMut() -> Option<GameState>>,
    tracker: ShiftlockTracker,
    ult_on: Arc<AtomicBool>,
    settings: Box<dyn Fn() -> Settings>,
    gate: Box<dyn Fn() -> bool>,
    cursor_on_roblox: Box<dyn Fn() -> bool>,
    sink: Option<Box<dyn FnMut(Op)>>,
    shiftlock: Arc<AtomicI8>,
    x1_down: Arc<AtomicBool>,
    jumpset_held: Option<Key>, // the exact key pressed, so a settings change can't strand it
    after_air: Plan,           // rest of a plan parked at Op::WaitAir
    shift_back: bool,          // a Shift tap owed by Op::ShiftBackOnAir
    shift_back_at: Instant,
    x2_held: bool,
    x2_waiting: bool,
    x2_cancelled: bool,
    air: bool,
    counter_armed: bool, // auto counter spike: once per airtime
    x2_step_at: Instant,
}

impl Engine {
    /// wake must be an auto-reset event (hooks set it). gate = updater Ready && Roblox foreground.
    /// cursor_on_roblox is checked before every injected mouse press/tap; when false the op is skipped.
    /// shiftlock receives the tracker's state (-1 unknown, 0 off, 1 on) for the dashboard.
    /// x1_down is the physical X1 button as the mouse hook last saw it (it swallows X1, so GetAsyncKeyState can't).
    #[allow(clippy::too_many_arguments)] // every dependency is injected for the tests
    pub fn new(
        queue: Arc<InputQueue>,
        wake: Arc<WakeEvent>,
        sample: Box<dyn FnMut() -> Option<GameState>>,
        tracker: ShiftlockTracker,
        ult_on: Arc<AtomicBool>,
        settings: Box<dyn Fn() -> Settings>,
        gate: Box<dyn Fn() -> bool>,
        cursor_on_roblox: Box<dyn Fn() -> bool>,
        shiftlock: Arc<AtomicI8>,
        x1_down: Arc<AtomicBool>,
    ) -> Self {
        shiftlock.store(-1, Relaxed);
        Self {
            queue,
            wake,
            sample,
            tracker,
            ult_on,
            settings,
            gate,
            cursor_on_roblox,
            sink: None,
            shiftlock,
            x1_down,
            jumpset_held: None,
            after_air: Plan::new(),
            shift_back: false,
            shift_back_at: Instant::now(),
            x2_held: false,
            x2_waiting: false,
            x2_cancelled: false,
            air: false,
            counter_armed: false,
            x2_step_at: Instant::now(),
        }
    }

    pub fn run(&mut self, stop: &AtomicBool) {
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        }
        while !stop.load(Relaxed) {
            // X2 held: poll at 1 ms so a landing re-arms the jump promptly (main sets timeBeginPeriod(1)).
            // Airborne with auto counter on: 1 ms too, so an incoming ball is caught within reach.
            let fast = self.x2_held
                || self.shift_back
                || !self.after_air.is_empty()
                || (self.air && (self.settings)().counter_on);
            self.wake.wait(if fast { 1 } else { 10 });
            self.step();
        }
        self.abort();
    }

    /// One wake: sample, drain events, step tracker, act. run() calls it; tests drive it directly.
    pub fn step(&mut self) {
        let mut evs: Vec<InputEvent> = Vec::new();
        while evs.len() < 256
            && let Some(e) = self.queue.pop()
        {
            evs.push(e);
        }

        let st = (self.sample)();
        let gated = (self.gate)();
        let ok = st.is_some() && gated;
        let live = ok && !st.as_ref().unwrap().typing;
        // our own Shift taps are injected and always paired, so they are skipped
        let shift_edge = evs.iter().any(|e| e.kind == EventType::ShiftDown && !e.injected);
        let sl = self.tracker.step(st.as_ref().and_then(|s| s.cam), shift_edge && live);
        self.shiftlock.store(sl.map_or(-1, i8::from), Relaxed);
        if !ok {
            self.abort();
        }

        let view = |e: &Self| to_view(st.as_ref().unwrap(), sl, e.ult_on.load(Relaxed));
        for e in &evs {
            match e.kind {
                EventType::ToggleUltKey => {
                    if gated {
                        // F4 typed in another window must not flip it
                        self.ult_on.store(!self.ult_on.load(Relaxed), Relaxed);
                    }
                }
                EventType::ShiftDown => {}
                EventType::X1Down => {
                    if self.x2_held {
                        self.x2_cancelled = true; // X1 during an X2 hold drops X2's release actions
                    }
                    if live && self.jumpset_held.is_none() {
                        let p = on_x1_down(&view(self), self.x2_held);
                        self.exec(&p);
                    }
                }
                EventType::X1Up => {
                    // releasing is always allowed
                    self.after_air.clear();
                    let p = on_x1_up(self.jumpset_held.is_some());
                    self.exec(&p);
                }
                EventType::X2Down => {
                    self.x2_held = live;
                    self.x2_waiting = false;
                    self.x2_cancelled = false;
                    if live {
                        self.x2_tick(&view(self));
                    }
                }
                EventType::X2Up => {
                    if self.x2_held && live && !self.x2_cancelled {
                        let p = on_x2_up(&view(self));
                        self.exec(&p);
                    }
                    self.x2_held = false;
                    self.x2_cancelled = false;
                }
                EventType::ResetKey => {
                    if live {
                        self.exec(&reset_plan());
                    }
                }
            }
        }
        // Typing started mid-hold, or an X1Up was dropped by a full queue: never leave the jumpset key held.
        if self.jumpset_held.is_some() && (!live || !self.x1_down.load(Relaxed)) {
            self.emit(Op::ReleaseJumpset, &Settings::default()); // release uses the stored key, not settings
        }
        // Everything below needs live (checked on each step, then returns).
        if !live {
            return;
        }
        let st = st.as_ref().unwrap();
        if !self.after_air.is_empty() && st.footing == Footing::Air {
            let p = std::mem::take(&mut self.after_air);
            self.exec(&p);
        }
        // shortcut: 1 s fallback if the boom jump never takes off; tune if its wind-up turns out longer.
        // gate asked again: an exec above may have lost focus, and a dropped tap would lose the owed toggle
        if self.shift_back
            && (st.footing == Footing::Air || self.shift_back_at.elapsed() >= Duration::from_secs(1))
            && (self.gate)()
        {
            self.shift_back = false;
            self.exec(&[Op::TapShift]);
        }
        if self.x2_held {
            self.x2_tick(&view(self));
        }
        self.air = st.footing == Footing::Air;
        if st.footing == Footing::Ground {
            self.counter_armed = true;
        }
        if self.air
            && self.counter_armed
            && self.jumpset_held.is_none()
            && let Some(ball) = st.ball
        {
            let s = (self.settings)();
            let p = counter_step(&view(self), &ball, if s.counter_on { s.counter_reach } else { 0 });
            if !p.is_empty() {
                self.counter_armed = false;
                self.exec(&p);
            }
        }
    }

    fn x2_tick(&mut self, v: &MacroView) {
        let now = Instant::now();
        let rearm = Duration::from_millis((self.settings)().x2_rearm_ms.max(0) as u64);
        if self.x2_waiting && (!v.ground || !v.ready || now - self.x2_step_at >= rearm) {
            self.x2_waiting = false;
        }
        if self.x2_waiting || self.shift_back {
            return; // never stack a second shift toggle on an owed one
        }
        let p = x2_ground_step(v);
        if p.is_empty() {
            return;
        }
        self.exec(&p);
        self.x2_waiting = true;
        self.x2_step_at = Instant::now(); // re-arm timer counts from the end of the (shift) sequence
    }

    /// Releases held keys (jumpset). Called on gate loss, unreadable state and stop.
    pub fn abort(&mut self) {
        self.x2_held = false;
        self.x2_waiting = false;
        self.air = false;
        self.after_air.clear();
        if self.jumpset_held.is_some() {
            self.emit(Op::ReleaseJumpset, &Settings::default()); // release uses the stored key, not settings
        }
        // Undo the owed shift lock toggle only while Roblox is foreground; else the tap would land in another window,
        // so it stays owed and step() sends it once live again.
        if self.shift_back && (self.gate)() {
            self.shift_back = false;
            self.emit(Op::TapShift, &Settings::default());
        }
    }

    /// For the dashboard; any thread reads the same value through shiftlock_of.
    pub fn shiftlock(&self) -> Option<bool> {
        shiftlock_of(&self.shiftlock)
    }

    /// Test seam: ops go to the sink instead of being sent, and waits are not slept.
    pub fn set_sink_for_tests(&mut self, sink: impl FnMut(Op) + 'static) {
        self.sink = Some(Box::new(sink));
    }

    fn exec(&mut self, plan: &[Op]) {
        if plan.is_empty() {
            return;
        }
        let s = (self.settings)();
        let mut shift_odd = false;
        for (i, &op) in plan.iter().enumerate() {
            if op == Op::WaitAir {
                self.after_air = plan[i + 1..].to_vec(); // step() resumes it once airborne
                return;
            }
            if op == Op::ShiftBackOnAir {
                // the toggle back is now owed by step()/abort()
                self.shift_back = true;
                self.shift_back_at = Instant::now();
                shift_odd = false;
                continue;
            }
            if !(self.gate)() {
                if shift_odd {
                    // undo the half-done shiftlock toggle once Roblox is back (step() sends it on the first live step)
                    self.shift_back = true;
                    self.shift_back_at = Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .unwrap_or_else(Instant::now);
                }
                self.abort();
                return;
            }
            self.emit(op, &s);
            if op == Op::TapShift {
                shift_odd = !shift_odd;
            }
        }
    }

    fn emit(&mut self, op: Op, s: &Settings) {
        // A mouse click with the cursor off Roblox would land elsewhere: skip it (a skipped press is never held).
        let off_target = |e: &Self, k: &Key| k.kind == KeyKind::Mouse && !(e.cursor_on_roblox)();
        let ult_key = || resolve_key(&s.ultimate_key, "LCTRL").0;
        let spike_key = || resolve_key(&s.spike_key, "M1").0;
        if op == Op::PressJumpset {
            let k = resolve_key(&s.jumpset_key, "E").0;
            if off_target(self, &k) {
                return;
            }
            self.jumpset_held = Some(k);
            HELD_JUMPSET.store(pack_key(&k), Relaxed);
        }
        if (op == Op::TapUltimate && off_target(self, &ult_key()))
            || (op == Op::TapSpike && off_target(self, &spike_key()))
        {
            return;
        }
        if op == Op::ReleaseJumpset && self.jumpset_held.is_none() {
            return;
        }
        if let Some(sink) = &mut self.sink {
            sink(op);
        } else {
            let ms = |v: i32| Duration::from_millis(v.max(0) as u64);
            let hold = ms(s.tap_hold_ms);
            match op {
                Op::TapSpace => tap(&SPACE, hold),
                Op::PressJumpset => press(self.jumpset_held.as_ref().unwrap()),
                Op::ReleaseJumpset => release(self.jumpset_held.as_ref().unwrap()),
                Op::TapShift => tap(&SHIFT, hold),
                Op::WaitShiftDelay => precise_wait(ms(s.shift_delay_ms)),
                Op::TapUltimate => tap(&ult_key(), hold),
                Op::WaitUltSpikeGap => precise_wait(ms(s.ult_spike_gap_ms)),
                Op::TapSpike => tap(&spike_key(), hold),
                Op::TapEsc => tap(&ESC, hold),
                Op::TapR => tap(&R, hold),
                Op::TapEnter => tap(&ENTER, hold),
                Op::WaitResetGap => precise_wait(ms(s.reset_gap_ms)),
                Op::WaitAir | Op::ShiftBackOnAir => {} // handled by exec()
            }
        }
        if op == Op::ReleaseJumpset {
            self.jumpset_held = None;
            HELD_JUMPSET.store(0, Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use handy_sdk::input::key::{MOUSE_LEFT, key_from_label};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::{Mutex, MutexGuard};

    use crate::game::ball::BallInfo;
    use crate::game::shiftlock::CameraSample;

    // HELD_JUMPSET is process-wide and tests run in parallel: engine tests take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    struct Rig {
        q: Arc<InputQueue>,
        ult_on: Arc<AtomicBool>,
        state: Rc<RefCell<Option<GameState>>>,
        gate: Rc<Cell<bool>>,
        cursor_on_roblox: Rc<Cell<bool>>,
        settings: Rc<RefCell<Settings>>,
        ops: Rc<RefCell<Vec<Op>>>,
        x1_down: Arc<AtomicBool>,
        engine: Engine,
        _serial: MutexGuard<'static, ()>,
    }

    impl Rig {
        fn new() -> Self {
            let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            let q = Arc::new(InputQueue::new());
            let ult_on = Arc::new(AtomicBool::new(false));
            let state = Rc::new(RefCell::new(Some(GameState {
                footing: Footing::Ground,
                cooldown: Some(false),
                cam: Some(CameraSample {
                    right_offset: 2.0,
                    distance: 10.0,
                }), // third person, shiftlock on
                ..GameState::default()
            })));
            let gate = Rc::new(Cell::new(true));
            let cursor_on_roblox = Rc::new(Cell::new(true));
            let settings = Rc::new(RefCell::new(Settings::default()));
            let ops = Rc::new(RefCell::new(Vec::new()));
            let x1_down = Arc::new(AtomicBool::new(false));
            let (st, g, c, se) = (state.clone(), gate.clone(), cursor_on_roblox.clone(), settings.clone());
            let mut engine = Engine::new(
                q.clone(),
                Arc::new(WakeEvent::new().unwrap()),
                Box::new(move || st.borrow().clone()),
                ShiftlockTracker::default(),
                ult_on.clone(),
                Box::new(move || se.borrow().clone()),
                Box::new(move || g.get()),
                Box::new(move || c.get()),
                Arc::new(AtomicI8::new(-1)),
                x1_down.clone(),
            );
            let o = ops.clone();
            engine.set_sink_for_tests(move |op| o.borrow_mut().push(op));
            Self {
                q,
                ult_on,
                state,
                gate,
                cursor_on_roblox,
                settings,
                ops,
                x1_down,
                engine,
                _serial: serial,
            }
        }
        fn send(&mut self, t: EventType) {
            self.send_injected(t, false);
        }
        fn send_injected(&mut self, t: EventType, injected: bool) {
            if !injected && (t == EventType::X1Down || t == EventType::X1Up) {
                self.x1_down.store(t == EventType::X1Down, Relaxed); // as the mouse hook does
            }
            self.q.push(InputEvent {
                kind: t,
                qpc: 0,
                injected,
            });
            self.engine.step();
        }
        fn st(&self) -> std::cell::RefMut<'_, GameState> {
            std::cell::RefMut::map(self.state.borrow_mut(), |s| s.as_mut().unwrap())
        }
        fn ops(&self) -> Vec<Op> {
            self.ops.borrow().clone()
        }
    }

    fn boom_rig() -> Rig {
        let r = Rig::new();
        {
            let mut s = r.st();
            s.cam = Some(CameraSample {
                right_offset: 0.0,
                distance: 10.0,
            }); // third person, shiftlock off
            s.ult_name = "BOOM JUMP".into();
            s.ult_percent = Some(100.0);
        }
        r.ult_on.store(true, Relaxed);
        r
    }

    #[test]
    fn engine_x1_press_release() {
        let mut r = Rig::new();
        r.send(EventType::X1Down);
        assert_eq!(r.ops(), vec![Op::TapSpace]); // waits until airborne
        r.engine.step();
        assert_eq!(r.ops().len(), 1);
        r.st().footing = Footing::Air;
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::TapSpace, Op::PressJumpset]);
        r.send(EventType::X1Up);
        assert_eq!(r.ops(), vec![Op::TapSpace, Op::PressJumpset, Op::ReleaseJumpset]);
        r.send(EventType::X1Up); // nothing held -> nothing sent
        assert_eq!(r.ops().len(), 3);
    }

    #[test]
    fn engine_x1_wait_air_cancelled_by_release_or_abort() {
        let mut r = Rig::new();
        r.send(EventType::X1Down);
        r.send(EventType::X1Up); // released before takeoff: nothing pressed, nothing released
        r.st().footing = Footing::Air;
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::TapSpace]);
        drop(r);

        let mut s = Rig::new();
        s.send(EventType::X1Down);
        s.gate.set(false);
        s.engine.step();
        s.gate.set(true);
        s.st().footing = Footing::Air;
        s.engine.step();
        assert_eq!(s.ops(), vec![Op::TapSpace]);
    }

    #[test]
    fn engine_releases_on_abort() {
        let mut r = Rig::new();
        r.st().footing = Footing::Air;
        r.send(EventType::X1Down);
        r.gate.set(false);
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::PressJumpset, Op::ReleaseJumpset]);
        r.gate.set(true);
        r.send(EventType::X1Up); // already released
        assert_eq!(r.ops().len(), 2);
        drop(r);

        let mut s = Rig::new(); // unreadable state also aborts
        s.st().footing = Footing::Air;
        s.send(EventType::X1Down);
        *s.state.borrow_mut() = None;
        s.engine.step();
        assert_eq!(s.ops(), vec![Op::PressJumpset, Op::ReleaseJumpset]);
    }

    #[test]
    fn engine_crash_release_key_pack_roundtrip() {
        assert!(unpack_key(0).is_none());
        let kb = |scan, extended| Key {
            kind: KeyKind::Keyboard,
            scan,
            extended,
            mouse_button: 0,
        };
        let x2 = Key {
            kind: KeyKind::Mouse,
            scan: 0,
            extended: false,
            mouse_button: 5,
        };
        for k in [SHIFT, SPACE, MOUSE_LEFT, kb(0x1D, true), kb(0, false), x2] {
            assert_ne!(pack_key(&k), 0);
            assert_eq!(unpack_key(pack_key(&k)), Some(k));
        }
    }

    #[test]
    fn engine_publishes_held_jumpset_for_crash_release() {
        let mut r = Rig::new();
        r.st().footing = Footing::Air;
        r.send(EventType::X1Down);
        assert_eq!(held_jumpset_key(), key_from_label("E")); // the default jumpset key
        r.send(EventType::X1Up);
        assert!(held_jumpset_key().is_none());

        r.send(EventType::X1Down); // abort clears it too
        assert!(held_jumpset_key().is_some());
        r.gate.set(false);
        r.engine.step();
        assert!(held_jumpset_key().is_none());
    }

    #[test]
    fn engine_shift_back_when_gate_lost_mid_sequence() {
        let mut r = Rig::new();
        r.st().cam = None; // shiftlock unknown -> shift sequence
        let (o, g) = (r.ops.clone(), r.gate.clone());
        r.engine.set_sink_for_tests(move |op| {
            o.borrow_mut().push(op);
            if op == Op::TapShift {
                g.set(false); // focus lost right after the first Shift tap
            }
        });
        r.send(EventType::X2Down);
        assert_eq!(r.ops(), vec![Op::TapShift]); // the undo would land in another window: owed
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::TapShift]);
        r.gate.set(true);
        r.engine.step(); // Roblox is back: undone at once, on the ground
        assert_eq!(r.ops(), vec![Op::TapShift, Op::TapShift]);
        r.engine.step();
        assert_eq!(r.ops().len(), 2);
    }

    #[test]
    fn engine_boom_jump_keeps_shiftlock_until_airborne() {
        let mut r = boom_rig();
        r.send(EventType::X2Down);
        let started = vec![Op::TapShift, Op::WaitShiftDelay, Op::TapUltimate];
        assert_eq!(r.ops(), started);
        r.engine.step(); // still on the ground: shift lock stays on
        assert_eq!(r.ops(), started);
        r.st().footing = Footing::Air;
        r.engine.step();
        assert!(r.ops().len() == 4 && r.ops().last() == Some(&Op::TapShift));
        r.engine.step(); // owed only once
        assert_eq!(r.ops().len(), 4);
        drop(r);

        let mut g = boom_rig(); // focus lost before takeoff: Shift is toggled back once Roblox is foreground again
        g.send(EventType::X2Down);
        g.gate.set(false);
        g.engine.step();
        assert_eq!(g.ops().len(), 3);
        g.gate.set(true);
        g.st().footing = Footing::Air;
        g.engine.step();
        assert!(g.ops().len() == 4 && g.ops().last() == Some(&Op::TapShift));
        drop(g);

        let mut u = boom_rig(); // state unreadable with Roblox foreground: toggled back right away, as before
        u.send(EventType::X2Down);
        *u.state.borrow_mut() = None;
        u.engine.step();
        assert!(u.ops().len() == 4 && u.ops().last() == Some(&Op::TapShift));
    }

    #[test]
    fn engine_releases_jumpset_when_typing_starts() {
        let mut r = Rig::new();
        r.st().footing = Footing::Air;
        r.send(EventType::X1Down);
        r.st().typing = true;
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::PressJumpset, Op::ReleaseJumpset]);
        assert!(held_jumpset_key().is_none());
        r.st().typing = false;
        r.send(EventType::X1Up); // already released
        assert_eq!(r.ops().len(), 2);
    }

    #[test]
    fn engine_releases_jumpset_when_x1_up_was_dropped() {
        let mut r = Rig::new();
        r.st().footing = Footing::Air;
        r.send(EventType::X1Down);
        r.engine.step(); // still held
        assert_eq!(r.ops(), vec![Op::PressJumpset]);
        r.x1_down.store(false, Relaxed); // the hook saw X1Up but the full queue dropped it
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::PressJumpset, Op::ReleaseJumpset]);
    }

    #[test]
    fn engine_ignores_when_typing() {
        let mut r = Rig::new();
        r.st().typing = true;
        r.send(EventType::X1Down);
        r.send(EventType::X2Down);
        r.send(EventType::X2Up);
        r.send(EventType::ResetKey);
        assert!(r.ops().is_empty());
        r.gate.set(false);
        r.st().typing = false;
        r.send(EventType::X1Down);
        r.send(EventType::ResetKey);
        assert!(r.ops().is_empty());
    }

    #[test]
    fn engine_x2_loop_rearms_after_leaving_ground() {
        let mut r = Rig::new();
        r.send(EventType::X2Down);
        assert_eq!(r.ops(), vec![Op::TapSpace]);
        r.engine.step(); // still ground-ready: waiting for the jump to register
        assert_eq!(r.ops().len(), 1);
        r.st().footing = Footing::Air;
        r.engine.step();
        assert_eq!(r.ops().len(), 1);
        r.st().footing = Footing::Ground;
        r.engine.step(); // landed -> jump again
        assert_eq!(r.ops(), vec![Op::TapSpace, Op::TapSpace]);
        r.st().footing = Footing::Air;
        r.engine.step();
        r.send(EventType::X2Up);
        assert_eq!(r.ops(), vec![Op::TapSpace, Op::TapSpace, Op::TapSpike]);
        r.engine.step(); // released: loop stopped
        assert_eq!(r.ops().len(), 3);
    }

    #[test]
    fn engine_x1_during_x2_cancels_x2_release() {
        let mut r = Rig::new();
        r.send(EventType::X2Down);
        r.st().footing = Footing::Air;
        r.send(EventType::X1Down); // air-ready: jumpset
        r.send(EventType::X1Up);
        r.send(EventType::X2Up); // cancelled: no spike
        assert_eq!(r.ops(), vec![Op::TapSpace, Op::PressJumpset, Op::ReleaseJumpset]);
        r.send(EventType::X2Down); // next hold is not cancelled
        r.send(EventType::X2Up);
        assert_eq!(r.ops().last(), Some(&Op::TapSpike));
        drop(r);

        let mut g = Rig::new(); // on ground with X2 held, X1 presses jumpset without its own space tap
        g.send(EventType::X2Down);
        g.send(EventType::X1Down);
        assert_eq!(g.ops(), vec![Op::TapSpace]); // X2's own jump only
        g.st().footing = Footing::Air;
        g.engine.step();
        assert_eq!(g.ops(), vec![Op::TapSpace, Op::PressJumpset]);
    }

    #[test]
    fn engine_toggle_ult_and_reset() {
        let mut r = Rig::new();
        r.send(EventType::ToggleUltKey);
        assert!(r.ult_on.load(Relaxed));
        r.send(EventType::ResetKey);
        assert_eq!(
            r.ops(),
            vec![Op::TapEsc, Op::WaitResetGap, Op::TapR, Op::WaitResetGap, Op::TapEnter]
        );
        r.gate.set(false); // Roblox not foreground: hotkeys do nothing
        r.send(EventType::ToggleUltKey);
        r.send(EventType::ResetKey);
        assert!(r.ult_on.load(Relaxed));
        assert_eq!(r.ops().len(), 5);
    }

    #[test]
    fn engine_mouse_ops_need_cursor_over_roblox() {
        let mut r = Rig::new();
        r.settings.borrow_mut().jumpset_key = "M2".into(); // spike key defaults to M1
        r.cursor_on_roblox.set(false);
        r.send(EventType::X1Down); // keyboard Space goes out; the mouse press is skipped and not held
        r.send(EventType::X1Up);
        r.st().footing = Footing::Air;
        r.send(EventType::X2Down);
        r.send(EventType::X2Up);
        assert_eq!(r.ops(), vec![Op::TapSpace]);
        r.cursor_on_roblox.set(true);
        r.send(EventType::X1Down);
        r.cursor_on_roblox.set(false);
        r.send(EventType::X1Up); // a held press is always released
        r.send(EventType::X2Down);
        r.cursor_on_roblox.set(true);
        r.send(EventType::X2Up);
        assert_eq!(
            r.ops(),
            vec![Op::TapSpace, Op::PressJumpset, Op::ReleaseJumpset, Op::TapSpike]
        );
    }

    #[test]
    fn engine_shiftlock_tracks_user_shift_only() {
        let mut r = Rig::new();
        r.engine.step();
        assert_eq!(r.engine.shiftlock(), Some(true));
        r.st().cam = Some(CameraSample {
            right_offset: 0.0,
            distance: 0.5,
        }); // first person: carry state, flip on Shift
        r.send_injected(EventType::ShiftDown, true); // our own injected Shift is ignored
        assert_eq!(r.engine.shiftlock(), Some(true));
        r.send(EventType::ShiftDown);
        assert_eq!(r.engine.shiftlock(), Some(false));
    }

    #[test]
    fn engine_counter_once_per_air() {
        let mut r = Rig::new();
        r.settings.borrow_mut().counter_on = true;
        r.st().ball = Some(BallInfo {
            dist: 5.0,
            toward_me: 30.0,
        });
        r.engine.step(); // on ground: arms, never fires
        assert!(r.ops().is_empty());
        r.st().footing = Footing::Air;
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::TapSpike]);
        r.engine.step(); // same jump: once only
        assert_eq!(r.ops().len(), 1);
        r.st().footing = Footing::Ground;
        r.engine.step();
        r.st().footing = Footing::Air;
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::TapSpike, Op::TapSpike]);
    }

    #[test]
    fn engine_counter_not_while_setting() {
        let mut r = Rig::new();
        r.settings.borrow_mut().counter_on = true;
        r.engine.step();
        r.st().footing = Footing::Air;
        r.send(EventType::X1Down); // jumpset held
        r.st().ball = Some(BallInfo {
            dist: 5.0,
            toward_me: 30.0,
        });
        r.engine.step();
        assert_eq!(r.ops(), vec![Op::PressJumpset]);
    }

    #[test]
    fn engine_counter_needs_ball() {
        let mut r = Rig::new();
        r.settings.borrow_mut().counter_on = true;
        r.engine.step();
        r.st().footing = Footing::Air;
        r.engine.step();
        assert!(r.ops().is_empty());
    }

    #[test]
    fn engine_counter_off_by_default() {
        let mut r = Rig::new();
        r.st().ball = Some(BallInfo {
            dist: 5.0,
            toward_me: 30.0,
        });
        r.engine.step();
        r.st().footing = Footing::Air;
        r.engine.step();
        assert!(r.ops().is_empty());
    }
}
