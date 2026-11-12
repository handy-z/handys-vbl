use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use handy_sdk::app::updater_thread::{Session, UpdaterHooks};
use handy_sdk::util::ini::Ini;

use super::config::{Settings, SettingsStore};
use crate::game::calibration::{ColorCalibrator, image_color_guess};
use crate::game::reader::{GameReader, Keybinds};

const ICON_COLOR: &str = "IconColor";

// One section per Roblox hash.
fn calibration(ini: &Path, hash: &str) -> Ini {
    Ini::with_section(ini, hash)
}
fn saved_icon_color(ini: &Path, hash: &str) -> Option<u32> {
    let v = calibration(ini, hash).get_int("iconColor", 0) as u32;
    (v != 0).then_some(v)
}
fn save_icon_color(ini: &Path, hash: &str, off: u32) {
    calibration(ini, hash).put_int("iconColor", off as i32);
}

/// Applies only the game labels (+ keys_from_game=true) in place; true when anything changed.
pub fn apply_keybinds(s: &mut Settings, k: &Keybinds) -> bool {
    // Labels come from game memory and land in config.ini: only [A-Za-z0-9]{1,16}, else keep the previous value.
    let label = |v: &str, prev: &str| {
        let ok = !v.is_empty() && v.len() <= 16 && v.bytes().all(|c| c.is_ascii_alphanumeric());
        if ok { v.to_owned() } else { prev.to_owned() }
    };
    let n = Settings {
        jumpset_key: label(&k.jumpset, &s.jumpset_key),
        ultimate_key: label(&k.ultimate, &s.ultimate_key),
        spike_key: label(&k.spike, &s.spike_key),
        keys_from_game: true,
        ..s.clone()
    };
    if n == *s {
        return false;
    }
    *s = n;
    true
}

/// The action icons' ImageColor3 offset found by updater_hooks().prepare; set on every Ready session.
pub fn icon_color(s: &Session) -> Option<u32> {
    s.calibrated.get(ICON_COLOR).map(|v| *v as u32)
}

/// A GameReader over s with its icon_color: the SessionReader<GameReader> factory.
pub fn make_reader(s: &Session) -> GameReader {
    GameReader::new(s.proc.clone(), s.offsets.clone(), icon_color(s))
}

// The reader references its session, so it is rebuilt (and the session kept alive) per Session.
#[derive(Default)]
struct KeySync {
    sess: Option<Arc<Session>>,
    reader: Option<GameReader>,
    last: Option<Instant>,
}

/// prepare: calibrates icon_color (persisted per Roblox hash in data_dir/calibration.ini).
/// tick: once a second copies the game's keybind labels into settings, saving data_dir/config.ini on change.
pub fn updater_hooks(settings: Arc<SettingsStore>, data_dir: &Path) -> UpdaterHooks {
    let cal_ini = data_dir.join("calibration.ini");
    let prepare = move |sess: &mut Session, hash: &str, wait: &dyn Fn(Duration) -> bool| -> bool {
        let guess = image_color_guess(&sess.offsets);
        let mut saved = saved_icon_color(&cal_ini, hash);
        let mut reader = GameReader::new(sess.proc.clone(), sess.offsets.clone(), Some(guess));
        let mut cal = ColorCalibrator::new(Some(guess));
        let found = loop {
            // A persisted offset is trusted only after it re-validates on all six icons.
            if let Some(off) = saved {
                let v = reader.valid_color_at(off);
                if v == Some(true) {
                    break off;
                }
                if v.is_some() {
                    saved = None;
                }
            }
            if saved.is_none() {
                if reader.valid_color_at(guess) == Some(true) {
                    save_icon_color(&cal_ini, hash, guess);
                    break guess;
                }
                if reader.feed_calibration(&mut cal) {
                    let off = cal.locked().unwrap();
                    save_icon_color(&cal_ini, hash, off);
                    break off;
                }
            }
            if !wait(Duration::from_millis(50)) {
                return false;
            }
        };
        sess.calibrated.insert(ICON_COLOR.into(), i64::from(found));
        true
    };

    let ks = Mutex::new(KeySync::default());
    let config_ini = data_dir.join("config.ini");
    let tick = move |s: &Arc<Session>| {
        let mut ks = ks.lock().unwrap();
        let now = Instant::now();
        if ks.last.is_some_and(|l| now - l < Duration::from_secs(1)) {
            return;
        }
        ks.last = Some(now);
        if !ks.sess.as_ref().is_some_and(|p| Arc::ptr_eq(p, s)) {
            ks.reader = None; // it references the old session, so it goes first
            ks.sess = Some(s.clone());
            ks.reader = Some(make_reader(s));
        }
        if let Some(k) = ks.reader.as_mut().and_then(GameReader::read_keybinds) {
            let mut changed = false;
            settings.update(|st| changed = apply_keybinds(st, &k));
            if changed {
                settings.save(&config_ini);
            }
        }
    };
    UpdaterHooks {
        prepare: Some(Box::new(prepare)),
        tick: Some(Box::new(tick)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::config::load_settings;

    fn kb(j: &str, u: &str, s: &str) -> Keybinds {
        Keybinds {
            jumpset: j.into(),
            ultimate: u.into(),
            spike: s.into(),
        }
    }

    #[test]
    fn updater_hooks_icon_color_reads_calibrated() {
        let mut s = Session::default();
        assert!(icon_color(&s).is_none());
        s.calibrated.insert("IconColor".into(), 0x9f0);
        assert_eq!(icon_color(&s).unwrap_or(0), 0x9f0);
    }

    #[test]
    fn updater_settings_store_updates_visible_across_threads() {
        let st = SettingsStore::new(Settings::default());
        assert_eq!(st.get().jumpset_key, "E");
        std::thread::scope(|sc| {
            sc.spawn(|| {
                st.update(|s| {
                    s.jumpset_key = "Q".into();
                    s.keys_from_game = true;
                })
            });
        });
        assert_eq!(st.get().jumpset_key, "Q");
        assert!(st.get().keys_from_game);
    }

    #[test]
    fn updater_keybind_sync_diff() {
        let mut cur = Settings::default(); // E / LCTRL / M1, not from game
        let same = kb("E", "LCTRL", "M1");
        assert!(apply_keybinds(&mut cur, &same) && cur.keys_from_game); // adopts the game as the source
        assert!(!apply_keybinds(&mut cur, &same)); // now in sync
        let before = cur.clone();
        assert!(apply_keybinds(&mut cur, &kb("Q", "LCTRL", "M2")));
        assert!(cur.jumpset_key == "Q" && cur.spike_key == "M2" && cur.ultimate_key == "LCTRL");
        assert_eq!(cur.shift_delay_ms, before.shift_delay_ms);
        assert!(apply_keybinds(&mut cur, &kb("F", "L CTRL\r\n[x]", "ABCDEFGHIJKLMNOPQ"))); // bad labels keep the previous value
        assert!(cur.jumpset_key == "F" && cur.ultimate_key == "LCTRL" && cur.spike_key == "M2");
        assert!(!apply_keybinds(&mut cur, &kb("", "", "")));
    }

    #[test]
    fn updater_keybind_sync_keeps_concurrent_timing_edit_and_saves() {
        let dir = std::env::temp_dir().join("vbl-updater-hooks-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let st = SettingsStore::new(Settings::default());
        st.update(|s| s.shift_delay_ms = 77); // dashboard edit landing before the sync applies
        st.update(|s| {
            apply_keybinds(s, &kb("Q", "R", "M2"));
        });
        let g = st.get();
        assert_eq!(g.shift_delay_ms, 77);
        assert_eq!(g.jumpset_key, "Q");
        st.save(&dir.join("config.ini"));
        let loaded = load_settings(&dir.join("config.ini"));
        assert!(loaded.shift_delay_ms == 77 && loaded.jumpset_key == "Q");
        let _ = std::fs::remove_dir_all(dir);
    }
}
