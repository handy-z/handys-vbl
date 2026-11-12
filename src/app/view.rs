use handy_sdk::app::updater_thread::{AppStatus, status_name_with};
use handy_sdk::input::key::{key_from_label, vk_name};
use handy_sdk::util::text::parse_bounded;

use super::config::Settings;
use crate::game::state::{Footing, GameState};

/// Display text shared by the dashboard window and the --status snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardView {
    pub status: String,
    pub shiftlock: String,
    pub footing: String,
    pub ultimate: String,
    pub jumpset_key: String,
    pub ultimate_key: String,
    pub spike_key: String,
    pub key_warning: String,
    pub reset_key: String,
    pub toggle_key: String,
}

pub fn footing_text(f: Footing, cooldown: Option<bool>) -> String {
    let s = match f {
        Footing::Unknown => return "Unknown".into(),
        Footing::Ground => "On ground",
        Footing::Air => "Airborne",
    };
    match cooldown {
        None => format!("{s} (cooldown unknown)"),
        Some(true) => format!("{s} (cooldown)"),
        Some(false) => s.into(),
    }
}

pub const FOOTING_WARNING: &str = "footing unreadable (Roblox update?)";

/// The action bar was found but no footing button reads visible: only a moved GUI or flag explains it.
pub fn footing_unreadable(g: Option<&GameState>) -> bool {
    g.is_some_and(|g| g.bar_found && g.footing == Footing::Unknown)
}

pub fn shiftlock_text(v: Option<bool>) -> String {
    match v {
        None => "unknown",
        Some(true) => "on",
        Some(false) => "off",
    }
    .into()
}

/// printf "%g": 6 significant digits, trailing zeros dropped, exponent form outside 1e-4 .. 1e6.
fn fmt_g(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let e = format!("{x:.5e}"); // the exponent after rounding to 6 digits, as %g decides it
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s.to_owned()
        }
    };
    if (-4..6).contains(&exp) {
        trim(&format!("{x:.*}", (5 - exp) as usize))
    } else {
        format!("{}e{}{:02}", trim(mant), if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

pub fn ultimate_text(name: &str, pct: Option<f64>) -> String {
    if name.is_empty() && pct.is_none() {
        return "unknown".into();
    }
    let p = pct.map_or_else(|| "?%".to_owned(), |p| format!("{}%", fmt_g(p)));
    format!("{} {p}", if name.is_empty() { "?" } else { name })
}

/// "E (game)", "E (fallback)", or "Q? (fallback: unmappable, using E)".
pub fn key_text(label: &str, fallback: &str, from_game: bool) -> String {
    let src = if from_game { "game" } else { "fallback" };
    if key_from_label(label).is_some() {
        return format!("{label} ({src})");
    }
    format!(
        "{} ({src}: unmappable, using {fallback})",
        if label.is_empty() { "(none)" } else { label }
    )
}

/// Non-empty when any of jumpset/ultimate/spike uses its fallback key.
pub fn keybind_warning(s: &Settings) -> String {
    let fallback = !s.keys_from_game
        || key_from_label(&s.jumpset_key).is_none()
        || key_from_label(&s.ultimate_key).is_none()
        || key_from_label(&s.spike_key).is_none();
    if fallback {
        "Fallback keys in use - open lobby Settings to sync keybinds".into()
    } else {
        String::new()
    }
}

/// Whole milliseconds in 0..1000, else None.
pub fn parse_timing_ms(s: &str) -> Option<i32> {
    parse_bounded(s, 0, 1000)
}

pub fn build_view(
    st: AppStatus,
    detail: &str,
    g: Option<&GameState>,
    shiftlock: Option<bool>,
    s: &Settings,
) -> DashboardView {
    let mut status = status_name_with(st, "Calibrating cooldown").to_owned();
    if !detail.is_empty() {
        status += " - ";
        status += detail;
    }
    DashboardView {
        status,
        shiftlock: shiftlock_text(shiftlock),
        footing: g.map_or_else(|| "Unknown".into(), |g| footing_text(g.footing, g.cooldown)),
        ultimate: g.map_or_else(|| "unknown".into(), |g| ultimate_text(&g.ult_name, g.ult_percent)),
        jumpset_key: key_text(&s.jumpset_key, "E", s.keys_from_game),
        ultimate_key: key_text(&s.ultimate_key, "LCTRL", s.keys_from_game),
        spike_key: key_text(&s.spike_key, "M1", s.keys_from_game),
        key_warning: keybind_warning(s),
        reset_key: vk_name(s.reset_vk),
        toggle_key: vk_name(s.toggle_ult_vk),
    }
}

/// Multi-line "key: value" snapshot written by `handys-vbl.exe --status <file>`.
pub fn snapshot_text(v: &DashboardView, s: &Settings) -> String {
    let ms = |x: i32| format!("{x} ms");
    let mut o = String::new();
    let mut line = |k: &str, val: &str| {
        o += &format!("{k}: {val}\n");
    };
    line("version", env!("CARGO_PKG_VERSION"));
    line("status", &v.status);
    line("shiftlock", &v.shiftlock);
    line("footing", &v.footing);
    line("ultimate", &v.ultimate);
    line("jumpset key", &v.jumpset_key);
    line("ultimate key", &v.ultimate_key);
    line("spike key", &v.spike_key);
    if !v.key_warning.is_empty() {
        line("keybind warning", &v.key_warning);
    }
    line("reset key", &v.reset_key);
    line("toggle key", &v.toggle_key);
    line("shift delay", &ms(s.shift_delay_ms));
    line("tap hold", &ms(s.tap_hold_ms));
    line("reset gap", &ms(s.reset_gap_ms));
    line("x2 re-arm", &ms(s.x2_rearm_ms));
    line("ult->spike gap", &ms(s.ult_spike_gap_ms));
    let reach = if s.counter_on && s.counter_reach != 0 {
        format!("{} studs", s.counter_reach)
    } else {
        "off".into()
    };
    line("counter reach", &reach);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_footing_unreadable_needs_bar_and_unknown() {
        let mut g = GameState::default();
        assert!(!footing_unreadable(None) && !footing_unreadable(Some(&g)));
        g.bar_found = true;
        assert!(footing_unreadable(Some(&g)));
        g.footing = Footing::Air;
        assert!(!footing_unreadable(Some(&g)));
    }

    #[test]
    fn view_footing_text() {
        assert_eq!(footing_text(Footing::Ground, Some(false)), "On ground");
        assert_eq!(footing_text(Footing::Ground, Some(true)), "On ground (cooldown)");
        assert_eq!(footing_text(Footing::Air, Some(false)), "Airborne");
        assert_eq!(footing_text(Footing::Air, Some(true)), "Airborne (cooldown)");
        assert_eq!(footing_text(Footing::Air, None), "Airborne (cooldown unknown)");
        assert_eq!(footing_text(Footing::Unknown, Some(true)), "Unknown");
    }

    #[test]
    fn view_shiftlock_and_ultimate_text() {
        assert_eq!(shiftlock_text(None), "unknown");
        assert_eq!(shiftlock_text(Some(true)), "on");
        assert_eq!(shiftlock_text(Some(false)), "off");
        assert_eq!(ultimate_text("BOOM JUMP", Some(100.0)), "BOOM JUMP 100%");
        assert_eq!(ultimate_text("CURVE SPIKE", Some(42.5)), "CURVE SPIKE 42.5%");
        assert_eq!(ultimate_text("", None), "unknown");
        assert_eq!(ultimate_text("X", None), "X ?%");
    }

    #[test]
    fn view_percent_formats_like_printf_g() {
        for (x, g) in [
            (33.33333333, "33.3333"),
            (0.5, "0.5"),
            (1234567.0, "1.23457e+06"),
            (0.00001, "1e-05"),
        ] {
            assert_eq!(fmt_g(x), g);
        }
    }

    #[test]
    fn view_key_text_source_and_warning() {
        assert_eq!(key_text("E", "E", true), "E (game)");
        assert_eq!(key_text("LCTRL", "LCTRL", false), "LCTRL (fallback)");
        assert_eq!(key_text("???", "M1", true), "??? (game: unmappable, using M1)");
        assert_eq!(key_text("", "E", true), "(none) (game: unmappable, using E)");
    }

    #[test]
    fn view_keybind_warning() {
        let mut s = Settings::default();
        assert!(!keybind_warning(&s).is_empty()); // defaults = fallback keys
        s.keys_from_game = true;
        assert!(keybind_warning(&s).is_empty());
        s.spike_key = "???".into(); // unmappable game label -> spike falls back to M1
        assert_eq!(
            keybind_warning(&s),
            "Fallback keys in use - open lobby Settings to sync keybinds"
        );
        let v = build_view(AppStatus::Ready, "", None, None, &s);
        assert_eq!(v.key_warning, keybind_warning(&s));
        assert!(snapshot_text(&v, &s).contains("keybind warning: Fallback keys in use"));
        s.spike_key = "M1".into();
        assert!(!snapshot_text(&build_view(AppStatus::Ready, "", None, None, &s), &s).contains("keybind warning"));
    }

    #[test]
    fn view_timing_validation() {
        assert_eq!(parse_timing_ms("0").unwrap_or(-1), 0);
        assert_eq!(parse_timing_ms("20").unwrap_or(-1), 20);
        assert_eq!(parse_timing_ms("1000").unwrap_or(-1), 1000);
        assert!(parse_timing_ms("1001").is_none());
        assert!(parse_timing_ms("").is_none());
        assert!(parse_timing_ms("-1").is_none());
        assert!(parse_timing_ms("1.5").is_none());
        assert!(parse_timing_ms(" 5").is_none());
        assert!(parse_timing_ms("99999999999").is_none());
    }

    #[test]
    fn view_build_and_snapshot() {
        let mut s = Settings {
            keys_from_game: true,
            ..Settings::default()
        };
        let g = GameState {
            footing: Footing::Ground,
            cooldown: Some(false),
            ult_name: "BOOM JUMP".into(),
            ult_percent: Some(100.0),
            ..GameState::default()
        };
        let v = build_view(AppStatus::Ready, "v1", Some(&g), Some(true), &s);
        assert_eq!(v.status, "Ready - v1");
        assert_eq!(v.footing, "On ground");
        assert_eq!(v.jumpset_key, "E (game)");
        assert_eq!(v.reset_key, "F1");
        assert_eq!(v.toggle_key, "F4");
        let none = build_view(AppStatus::RobloxClosed, "", None, None, &s);
        assert_eq!(none.status, "Waiting for Roblox");
        assert_eq!(none.footing, "Unknown");
        assert_eq!(none.ultimate, "unknown");
        let snap = snapshot_text(&v, &s);
        let head = format!(
            "version: {}
status: Ready - v1
",
            env!("CARGO_PKG_VERSION")
        );
        assert!(snap.starts_with(&head));
        assert!(snap.contains("shift delay: 20 ms\n"));
        assert!(snap.contains("counter reach: off\n"));
        s.counter_on = true;
        assert!(snapshot_text(&v, &s).contains("counter reach: 6 studs\n"));
    }
}
