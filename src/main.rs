#![windows_subsystem = "windows"]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use handy_sdk::app::args::arg_value;
use handy_sdk::app::crash_guard::on_crash;
use handy_sdk::app::self_update::{UpdateStatus, update_on_startup};
use handy_sdk::app::session_reader::SessionReader;
use handy_sdk::app::single_instance::SingleInstance;
use handy_sdk::app::status_snapshot::{snapshot_allowed, wait_for};
use handy_sdk::app::updater_thread::{AppStatus, Updater, production_smoke_test};
use handy_sdk::input::focus::cursor_over;
use handy_sdk::input::hooks::{Handlers, HotkeyEdge, KeyEvent, LowLevelHooks, MouseEvent};
use handy_sdk::roblox::events::{CHAT_KEYS, RobloxWatcher};
use handy_sdk::roblox::http::win_http_get;
use handy_sdk::roblox::updater::HttpGet;
use handy_sdk::util::event_bus::EventBus;
use handy_sdk::util::paths::{app_data_dir, set_app_name};
use handy_sdk::util::thread::StopThread;
use handy_sdk::util::timing::TimerResolution;
use handys_vbl::app::config::{SettingsStore, load_settings};
use handys_vbl::app::dashboard::{DASHBOARD_CLASS, DashboardDeps, run_dashboard};
use handys_vbl::app::offset_keys::required_keys;
use handys_vbl::app::updater_hooks::{make_reader, updater_hooks};
use handys_vbl::app::view::{build_view, snapshot_text};
use handys_vbl::game::shiftlock::ShiftlockTracker;
use handys_vbl::r#macro::engine::{Engine, WakeEvent, release_held_keys};
use handys_vbl::r#macro::input_rules::{
    EventType, InputEvent, InputQueue, XSwallowState, classify_key, classify_mouse,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWDEFAULT;

const MUTEX: &str = "Local\\handys-vbl";

fn http() -> HttpGet {
    Arc::new(win_http_get)
}

/// A HotkeyEdge in one atomic word (down << 32 | last_down_ms), so the Fn hook callback can step it lock-free.
#[derive(Default)]
struct AtomicEdge(AtomicU64);

impl AtomicEdge {
    fn get(&self) -> HotkeyEdge {
        let v = self.0.load(Relaxed);
        HotkeyEdge {
            down: v >> 32 != 0,
            last_down_ms: v as u32,
        }
    }
    fn set(&self, e: HotkeyEdge) {
        self.0
            .store(u64::from(e.down) << 32 | u64::from(e.last_down_ms), Relaxed);
    }
}

// `--status <file>`: read-only snapshot (no hooks, no input). Waits for Ready (+ one keybind sync) or 15 s.
fn status_snapshot(out: &Path) -> i32 {
    if !snapshot_allowed(MUTEX, "handy's vbl", out) {
        return 1;
    }
    let store = Arc::new(SettingsStore::new(load_settings(&app_data_dir().join("config.ini"))));
    let updater = Arc::new(
        Updater::new(http(), production_smoke_test, app_data_dir(), required_keys())
            .with_hooks(updater_hooks(store.clone(), &app_data_dir())),
    );
    let thread = {
        let u = updater.clone();
        StopThread::spawn(move |st| u.run(st))
    };
    let mut reader = SessionReader::for_updater(&updater, |s| make_reader(s));
    let mut tracker = ShiftlockTracker::default();
    let mut g = None;
    let mut shiftlock = None;
    let mut ready_at: Option<Instant> = None;
    wait_for(
        || {
            g = reader.get().and_then(|r| r.sample());
            shiftlock = tracker.step(g.as_ref().and_then(|g| g.cam), false);
            if updater.status() != AppStatus::Ready {
                return false;
            }
            ready_at.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(1500)
        },
        Duration::from_secs(15),
        Duration::from_millis(50),
    );
    let st = updater.status();
    let view = build_view(st, &updater.detail(), g.as_ref(), shiftlock, &store.get());
    let _ = std::fs::write(out, snapshot_text(&view, &store.get()));
    thread.request_stop();
    drop(thread);
    if st == AppStatus::Ready { 0 } else { 1 }
}

fn run() -> i32 {
    set_app_name("handy's vbl");
    on_crash(release_held_keys); // a crash while X1 is held must not leave the jumpset key down in Roblox
    if let Some(out) = arg_value("--status") {
        return if out.is_empty() {
            2
        } else {
            status_snapshot(Path::new(&out))
        };
    }

    // Only builds made by release.cmd update themselves (dev builds and build.cmd never do). Skipped while another
    // copy runs: its exe is in use and it would be replaced under it.
    let update = (option_env!("HANDY_RELEASE").is_some() && !SingleInstance::is_running(MUTEX))
        .then(|| update_on_startup("handy-z", "handys-vbl", env!("CARGO_PKG_VERSION")));
    if update == Some(UpdateStatus::Restarting) {
        return 0; // the new version is starting; exit before anything else runs
    }

    let mut instance = SingleInstance::new(MUTEX, DASHBOARD_CLASS);
    if !instance.acquire() {
        return 0; // brought the running copy's window up
    }
    let _timer = TimerResolution::new(); // 1 ms engine waits and precise_wait

    let ini = app_data_dir().join("config.ini");
    let store = Arc::new(SettingsStore::new(load_settings(&ini)));
    store.save(&ini); // first run: write the defaults so config.ini exists
    let s0 = store.get();
    let ult_on = Arc::new(AtomicBool::new(true)); // ultimate is on at every startup
    let roblox_foreground = Arc::new(AtomicBool::new(false));
    let reset_vk = Arc::new(AtomicU32::new(s0.reset_vk));
    let toggle_vk = Arc::new(AtomicU32::new(s0.toggle_ult_vk));

    let queue = Arc::new(InputQueue::new());
    let wake = Arc::new(WakeEvent::new().expect("CreateEventW")); // auto-reset, as Engine requires
    let updater = Arc::new(
        Updater::new(http(), production_smoke_test, app_data_dir(), required_keys())
            .with_hooks(updater_hooks(store.clone(), &app_data_dir())),
    );
    let bus = EventBus::new();
    let shiftlock = Arc::new(AtomicI8::new(-1));
    let x1_down = Arc::new(AtomicBool::new(false)); // physical X1, written by the mouse hook

    // Hook callbacks: O(1), atomics only. Touched only on the hook thread.
    let key = {
        let (queue, wake, reset_vk, toggle_vk) = (queue.clone(), wake.clone(), reset_vk.clone(), toggle_vk.clone());
        let (reset_edge, toggle_edge) = (AtomicEdge::default(), AtomicEdge::default());
        move |e: &KeyEvent| {
            let (mut r, mut t) = (reset_edge.get(), toggle_edge.get());
            let c = classify_key(
                e.msg,
                e.vk,
                reset_vk.load(Relaxed),
                toggle_vk.load(Relaxed),
                e.time,
                &mut r,
                &mut t,
                e.modifier_down,
            );
            reset_edge.set(r);
            toggle_edge.set(t);
            if let Some(kind) = c {
                queue.push(InputEvent {
                    kind,
                    qpc: e.qpc,
                    injected: e.injected,
                }); // full queue: drop
                wake.set();
            }
            false // keys always pass through
        }
    };
    let mouse = {
        let (queue, wake, fg, x1) = (queue.clone(), wake.clone(), roblox_foreground.clone(), x1_down.clone());
        let xs = XSwallowState::default();
        move |e: &MouseEvent| {
            let Some(t) = classify_mouse(e.msg, e.mouse_data) else {
                return false;
            };
            if !e.injected && (t == EventType::X1Down || t == EventType::X1Up) {
                x1.store(t == EventType::X1Down, Relaxed); // kept even when the queue drops the event
            }
            queue.push(InputEvent {
                kind: t,
                qpc: e.qpc,
                injected: e.injected,
            }); // full queue: drop
            wake.set();
            let is_down = t == EventType::X1Down || t == EventType::X2Down;
            xs.decide(t, is_down && fg.load(Relaxed), e.injected || e.ours)
        }
    };
    let mut hooks = LowLevelHooks::new(Handlers {
        key: Some(Box::new(key)),
        mouse: Some(Box::new(mouse)),
    });

    let updater_thread = {
        let u = updater.clone();
        StopThread::spawn(move |st| u.run(st))
    };
    let engine_thread = {
        let (u, store, queue, wake) = (updater.clone(), store.clone(), queue.clone(), wake.clone());
        let (ult_on, fg, shiftlock) = (ult_on.clone(), roblox_foreground.clone(), shiftlock.clone());
        StopThread::spawn(move |stop| {
            // engine thread only; chat is the only group the engine reads
            let watcher = RobloxWatcher::for_updater(&u, bus, &CHAT_KEYS);
            let mut reader = SessionReader::for_updater(&u, |s| make_reader(s)); // engine thread only
            let (gate_u, cursor_u) = (u.clone(), u);
            let mut engine = Engine::new(
                queue,
                wake,
                Box::new(move || {
                    watcher.poll();
                    let mut g = reader.get().and_then(|r| r.sample())?;
                    // unreadable chat counts as typing, so no keys are sent (as fisch and 99n)
                    g.typing = watcher.chat_focused().unwrap_or(true);
                    Some(g)
                }),
                ShiftlockTracker::default(),
                ult_on,
                Box::new(move || store.get()),
                Box::new(move || fg.load(Relaxed) && gate_u.status() == AppStatus::Ready),
                Box::new(move || cursor_u.session().is_some_and(|s| cursor_over(s.proc.pid()))),
                shiftlock,
                x1_down,
            );
            engine.run(stop);
        })
    };
    let hooks_ok = hooks.start();

    let deps = DashboardDeps {
        updater: updater.clone(),
        shiftlock,
        ult_on,
        roblox_foreground,
        reset_vk,
        toggle_vk,
        settings: store.clone(),
        config_ini: ini.clone(),
        hooks_ok,
        update,
    };
    let rc = run_dashboard(&deps, SW_SHOWDEFAULT);

    hooks.stop();
    engine_thread.request_stop(); // Engine::run ends with abort(): held keys are released
    wake.set();
    drop(engine_thread); // joins
    store.save(&ini);
    // The Updater join can wait out a WinHTTP timeout (up to 10 s): let a relaunch start meanwhile.
    instance.release();
    drop(updater_thread); // stops and joins
    rc
}

fn main() {
    std::process::exit(run());
}
