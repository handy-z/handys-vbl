use std::path::Path;

use handy_sdk::util::ini::Ini;
use handy_sdk::util::shared_settings::{SaveSettings, SharedSettings};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F1, VK_F4};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub reset_vk: u32,
    pub toggle_ult_vk: u32,
    pub ultimate_on: bool, // not persisted: ultimate is on at every startup
    pub jumpset_key: String,
    pub ultimate_key: String,
    pub spike_key: String,
    pub keys_from_game: bool,
    pub shift_delay_ms: i32,
    pub tap_hold_ms: i32,
    pub reset_gap_ms: i32,
    pub x2_rearm_ms: i32,
    pub ult_spike_gap_ms: i32,
    pub counter_on: bool,   // auto counter spike
    pub counter_reach: i32, // studs
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            reset_vk: u32::from(VK_F1.0),
            toggle_ult_vk: u32::from(VK_F4.0),
            ultimate_on: true,
            jumpset_key: "E".into(),
            ultimate_key: "LCTRL".into(),
            spike_key: "M1".into(),
            keys_from_game: false,
            shift_delay_ms: 20,
            tap_hold_ms: 10,
            reset_gap_ms: 30,
            x2_rearm_ms: 150,
            ult_spike_gap_ms: 0,
            counter_on: false,
            counter_reach: 6,
        }
    }
}

pub fn load_settings(file: &Path) -> Settings {
    let ini = Ini::new(file);
    let d = Settings::default();
    Settings {
        reset_vk: ini.get_int("resetVk", d.reset_vk as i32) as u32,
        toggle_ult_vk: ini.get_int("toggleUltVk", d.toggle_ult_vk as i32) as u32,
        ultimate_on: d.ultimate_on,
        jumpset_key: ini.get_str("jumpsetKey", &d.jumpset_key),
        ultimate_key: ini.get_str("ultimateKey", &d.ultimate_key),
        spike_key: ini.get_str("spikeKey", &d.spike_key),
        keys_from_game: ini.get_bool("keysFromGame", d.keys_from_game),
        shift_delay_ms: ini.get_int_clamped("shiftDelayMs", d.shift_delay_ms, 0, 1000),
        tap_hold_ms: ini.get_int_clamped("tapHoldMs", d.tap_hold_ms, 0, 1000),
        reset_gap_ms: ini.get_int_clamped("resetGapMs", d.reset_gap_ms, 0, 1000),
        x2_rearm_ms: ini.get_int_clamped("x2RearmMs", d.x2_rearm_ms, 0, 1000),
        ult_spike_gap_ms: ini.get_int_clamped("ultSpikeGapMs", d.ult_spike_gap_ms, 0, 1000),
        counter_on: ini.get_bool("counterOn", d.counter_on),
        counter_reach: ini.get_int_clamped("counterReach", d.counter_reach, 0, 20),
    }
}

pub fn save_settings(file: &Path, s: &Settings) {
    let ini = Ini::new(file);
    ini.put_int("resetVk", s.reset_vk as i32);
    ini.put_int("toggleUltVk", s.toggle_ult_vk as i32);
    ini.put_str("jumpsetKey", &s.jumpset_key);
    ini.put_str("ultimateKey", &s.ultimate_key);
    ini.put_str("spikeKey", &s.spike_key);
    ini.put_bool("keysFromGame", s.keys_from_game);
    ini.put_int("shiftDelayMs", s.shift_delay_ms);
    ini.put_int("tapHoldMs", s.tap_hold_ms);
    ini.put_int("resetGapMs", s.reset_gap_ms);
    ini.put_int("x2RearmMs", s.x2_rearm_ms);
    ini.put_int("ultSpikeGapMs", s.ult_spike_gap_ms);
    ini.put_bool("counterOn", s.counter_on);
    ini.put_int("counterReach", s.counter_reach);
}

impl SaveSettings for Settings {
    fn save(&self, ini: &Path) {
        save_settings(ini, self);
    }
}

/// Settings shared between the engine, dashboard and updater threads.
pub type SettingsStore = SharedSettings<Settings>;

#[cfg(test)]
mod tests {
    use super::*;
    use handy_sdk::util::paths::{app_data_dir, set_app_data_dir_for_tests};

    #[test]
    fn config_defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.reset_vk, u32::from(VK_F1.0));
        assert_eq!(s.toggle_ult_vk, u32::from(VK_F4.0));
        assert!(s.ultimate_on);
        assert_eq!(s.jumpset_key, "E");
        assert_eq!(s.ultimate_key, "LCTRL");
        assert_eq!(s.spike_key, "M1");
        assert_eq!(s.shift_delay_ms, 20);
        assert!(!s.counter_on); // auto counter spike off
        assert_eq!(s.counter_reach, 6);
    }

    #[test]
    fn config_roundtrip() {
        let dir = std::env::temp_dir().join("vbl-cfg-test's");
        set_app_data_dir_for_tests(Some(&dir));
        let ini = app_data_dir().join("settings.ini");
        let s = Settings {
            reset_vk: 0x71,      // VK_F2
            toggle_ult_vk: 0x74, // VK_F5
            jumpset_key: "Q".into(),
            ultimate_key: "R".into(),
            spike_key: "M2".into(),
            keys_from_game: true,
            shift_delay_ms: 25,
            tap_hold_ms: 11,
            reset_gap_ms: 31,
            x2_rearm_ms: 151,
            ult_spike_gap_ms: 5,
            counter_on: true,
            counter_reach: 7,
            ..Settings::default()
        };
        save_settings(&ini, &s);
        assert_eq!(load_settings(&ini), s);
        assert_eq!(load_settings(&dir.join("missing.ini")), Settings::default());
        set_app_data_dir_for_tests(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_clamps_ms_fields() {
        let dir = std::env::temp_dir().join("vbl-cfg-clamp");
        std::fs::create_dir_all(&dir).unwrap();
        let ini = dir.join("config.ini");
        save_settings(&ini, &Settings::default());
        let w = Ini::new(&ini); // the [macro] section
        w.put_str("shiftDelayMs", "-5");
        w.put_str("tapHoldMs", "99999");
        w.put_str("resetGapMs", "1001");
        w.put_str("x2RearmMs", "1000");
        w.put_str("ultSpikeGapMs", "-2147483648");
        w.put_str("counterReach", "21");
        let s = load_settings(&ini);
        assert_eq!(s.shift_delay_ms, 0);
        assert_eq!(s.tap_hold_ms, 1000);
        assert_eq!(s.reset_gap_ms, 1000);
        assert_eq!(s.x2_rearm_ms, 1000);
        assert_eq!(s.ult_spike_gap_ms, 0);
        assert_eq!(s.counter_reach, 20);
        let _ = std::fs::remove_dir_all(dir);
    }
}
