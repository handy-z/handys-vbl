use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

use handy_sdk::app::self_update::SelfUpdate;
use handy_sdk::app::session_reader::SessionReader;
use handy_sdk::app::updater_thread::{Updater, status_name_with, status_tone};
use handy_sdk::input::focus::ForegroundWatcher;
use handy_sdk::ui::Tone;
use handy_sdk::ui::dashboard::{Dashboard, Spec, check, note, number, page, section, value};
use handy_sdk::ui::overlay::StatusOverlay;
use handy_sdk::util::timing::Sustained;
use windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD;

use super::config::{Settings, SettingsStore};
use super::updater_hooks::make_reader;
use super::view::{FOOTING_WARNING, build_view, footing_unreadable};
use crate::r#macro::engine::{release_held_keys, shiftlock_of};

pub const DASHBOARD_CLASS: &str = "handys-vbl-dashboard";

const ID_SHIFT: i32 = 2;
const ID_FOOT: i32 = 3;
const ID_ULT: i32 = 4;
const ID_ULT_CHECK: i32 = 5;
const ID_JUMP: i32 = 6;
const ID_ULT_KEY: i32 = 7;
const ID_SPIKE: i32 = 8;
const ID_KEY_WARN: i32 = 9;
const ID_HINT: i32 = 10;
const ID_HOTKEYS: i32 = 11;
const ID_COUNTER_CHECK: i32 = 12;
const ID_EDIT0: i32 = 100;

fn timing(s: &mut Settings, i: usize) -> &mut i32 {
    match i {
        0 => &mut s.shift_delay_ms,
        1 => &mut s.tap_hold_ms,
        2 => &mut s.reset_gap_ms,
        3 => &mut s.x2_rearm_ms,
        4 => &mut s.ult_spike_gap_ms,
        _ => &mut s.counter_reach,
    }
}
const TIMINGS: usize = 6;

pub struct DashboardDeps {
    pub updater: Arc<Updater>,
    /// The engine's shiftlock state (see Engine::shiftlock).
    pub shiftlock: Arc<AtomicI8>,
    pub ult_on: Arc<AtomicBool>,
    /// written here (ForegroundWatcher + every tick), read by hooks/engine
    pub roblox_foreground: Arc<AtomicBool>,
    pub reset_vk: Arc<AtomicU32>,
    pub toggle_vk: Arc<AtomicU32>,
    pub settings: Arc<SettingsStore>,
    pub config_ini: PathBuf,
    /// LowLevelHooks::start() installed every hook
    pub hooks_ok: bool,
    pub update: Arc<SelfUpdate>,
}

/// Shows the dashboard and the "ultimate: on/off" overlay until the window is closed. UI thread only, one at a time.
pub fn run_dashboard(deps: &DashboardDeps, show: SHOW_WINDOW_CMD) -> i32 {
    let reader = RefCell::new(SessionReader::for_updater(&deps.updater, |s| make_reader(s))); // UI thread only
    // Hooks read the result from their LL callback, so it must be a plain atomic, never a live query.
    let footing_lost = RefCell::new(Sustained::default());
    let mut foreground = ForegroundWatcher::new(deps.roblox_foreground.clone(), None);
    let overlay = StatusOverlay::new();
    overlay.create();
    let first = Cell::new(true);
    let d = Dashboard::new(Spec {
        title: "handy's vbl".into(),
        badge: deps.update.badge().text,
        window_class: DASHBOARD_CLASS.into(),
        rows: vec![
            page("Play"),
            section("Game"),
            check(ID_ULT_CHECK, "Use ultimate"),
            check(ID_COUNTER_CHECK, "Auto counter spike"),
            value(ID_SHIFT, "Shiftlock"),
            value(ID_FOOT, "Footing"),
            value(ID_ULT, "Ultimate"),
            value(ID_HOTKEYS, "Reset key"),
            page("Settings"),
            section("Keybinds (read from the game)"),
            value(ID_JUMP, "Jumpset"),
            value(ID_ULT_KEY, "Ultimate"),
            value(ID_SPIKE, "Spike"),
            note(ID_KEY_WARN),
            section("Timings (ms, 0\u{2013}1000)"),
            number(ID_EDIT0, "Shift delay", 0, 1000),
            number(ID_EDIT0 + 1, "Tap hold", 0, 1000),
            number(ID_EDIT0 + 2, "Reset gap", 0, 1000),
            number(ID_EDIT0 + 3, "X2 re-arm", 0, 1000),
            number(ID_EDIT0 + 4, "Ult \u{2192} spike gap", 0, 1000),
            note(ID_HINT),
            section("Auto counter spike (airborne)"),
            number(ID_EDIT0 + 5, "Reach (studs)", 0, 20),
        ],
        hint_row: Some(ID_HINT),
        label_width: 110,
        value_width: 300,
        ..Default::default()
    });
    foreground.start(); // nothing pumps messages before run(), so this matches starting it after the callbacks
    let fg = &foreground;
    d.on_tick(|ui| {
        let b = deps.update.badge();
        ui.set_badge(&b.text, b.tone, &b.tip);
        let mut st = deps.settings.get();
        if first.replace(false) {
            // first tick runs right after the window is created
            for i in 0..TIMINGS {
                ui.set_number(ID_EDIT0 + i as i32, *timing(&mut st, i));
            }
        }
        let mut reader = reader.borrow_mut();
        let g = reader.get().and_then(|r| r.sample());
        fg.set_pid(reader.session().map_or(0, |s| s.proc.pid()));
        fg.refresh();

        deps.reset_vk.store(st.reset_vk, Relaxed);
        deps.toggle_vk.store(st.toggle_ult_vk, Relaxed);
        let ult = deps.ult_on.load(Relaxed);
        ui.set_check(ID_ULT_CHECK, ult);
        ui.set_check(ID_COUNTER_CHECK, st.counter_on);

        let status = deps.updater.status();
        let detail = deps.updater.detail();
        let v = build_view(status, &detail, g.as_ref(), shiftlock_of(&deps.shiftlock), &st);
        if deps.hooks_ok {
            ui.set_status_tip(&detail);
            ui.set_status(
                status_tone(status),
                status_name_with(status, "Calibrating cooldown"),
                "",
            );
        } else {
            ui.set_status_tip("");
            ui.set_status(
                Tone::Bad,
                "Hotkeys unavailable",
                "X1, X2 and the hotkeys do nothing: restart the app",
            );
        }
        ui.set_text(ID_SHIFT, &v.shiftlock);
        if footing_lost
            .borrow_mut()
            .update(footing_unreadable(g.as_ref()), Instant::now(), Duration::from_secs(2))
        {
            ui.set_text(ID_FOOT, FOOTING_WARNING);
        } else {
            ui.set_text(ID_FOOT, &v.footing);
        }
        ui.set_text(ID_ULT, &v.ultimate);
        ui.set_text(ID_ULT_CHECK, &format!("Use ultimate\tpress [{}]", v.toggle_key));
        ui.set_text(ID_JUMP, &v.jumpset_key);
        ui.set_text(ID_ULT_KEY, &v.ultimate_key);
        ui.set_text(ID_SPIKE, &v.spike_key);
        ui.set_text(ID_KEY_WARN, &v.key_warning);
        ui.set_text(ID_HOTKEYS, &format!("{} resets your character", v.reset_key));
        overlay.set(if ult { "Ultimate on" } else { "Ultimate off" }, ult);
    });
    d.on_badge_click(|ui| {
        if deps.update.request_restart() {
            ui.close();
        } else {
            handy_sdk::ui::open_url(&deps.update.release_url());
        }
    });
    d.on_check(|_, id, checked| {
        if id == ID_ULT_CHECK {
            deps.ult_on.store(checked, Relaxed);
        } else {
            deps.settings.update(|s| s.counter_on = checked);
            deps.settings.save(&deps.config_ini);
        }
    });
    d.on_number(|_, id, value| {
        let mut changed = false;
        deps.settings.update(|s| {
            let field = timing(s, (id - ID_EDIT0) as usize);
            changed = *field != value;
            *field = value;
        });
        if changed {
            deps.settings.save(&deps.config_ini);
        }
    });
    d.on_end_session(release_held_keys); // logoff/shutdown can end us without WM_DESTROY
    d.run(show)
}
