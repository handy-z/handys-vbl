//! Everything but the entry point, so it can be unit-tested.
#![cfg(windows)]

pub mod app {
    pub mod config;
    pub mod dashboard;
    pub mod offset_keys;
    pub mod updater_hooks;
    pub mod view;
}
pub mod game {
    pub mod ball;
    pub mod calibration;
    pub mod reader;
    pub mod shiftlock;
    pub mod state;
}
pub mod r#macro {
    pub mod decisions;
    pub mod engine;
    pub mod input_rules;
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_runner_works() {
        assert_eq!(1 + 1, 2);
    }
}

// Live tests: need a running Roblox client + network. Ignored; never run unasked.
#[cfg(test)]
mod live_tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use handy_sdk::app::updater_thread::{AppStatus, Updater, production_smoke_test};
    use handy_sdk::roblox::http::win_http_get;
    use handy_sdk::roblox::instance::{child_by_class, data_model, instance_class, resolve_path};
    use handy_sdk::roblox::memory::Memory;
    use handy_sdk::roblox::process::RobloxProcess;
    use handy_sdk::roblox::updater::{HttpGet, OffsetResult, OffsetStatus, OffsetStore};
    use handy_sdk::roblox::version::{
        VersionCandidate, VersionInputs, file_version_of, hash_from_api, player_guid_from_state, state_file_candidates,
        version_candidates,
    };
    use handy_sdk::util::paths::{app_data_dir, set_app_name};
    use handy_sdk::util::thread::StopThread;

    use crate::app::config::{Settings, SettingsStore};
    use crate::app::offset_keys::required_keys;
    use crate::app::updater_hooks::{icon_color, updater_hooks};
    use crate::game::calibration::image_color_guess;
    use crate::game::reader::GameReader;
    use crate::game::shiftlock::sample_camera;
    use crate::game::state::Footing;

    fn http() -> HttpGet {
        Arc::new(win_http_get)
    }

    // Live tests use the real app data dir (offset cache, calibration.ini) like the app does.
    fn find() -> Option<RobloxProcess> {
        set_app_name("handy's vbl");
        let p = RobloxProcess::find();
        if p.is_none() {
            println!("  SKIP: Roblox not running");
        }
        p
    }

    fn real_candidates(p: &RobloxProcess) -> Vec<VersionCandidate> {
        let exe_dir = Path::new(p.exe_path()).parent().unwrap_or(Path::new("")).to_path_buf();
        let fv = p.file_version().to_owned();
        let inp = VersionInputs {
            exe_path: p.exe_path().to_owned(),
            file_version: fv.clone(),
            state: Some(Box::new(move || {
                state_file_candidates(&exe_dir)
                    .iter()
                    .filter_map(|f| std::fs::read(f).ok())
                    .find_map(|j| player_guid_from_state(&String::from_utf8_lossy(&j)))
            })),
            api: Some(Box::new(move || {
                let r = win_http_get("https://clientsettings.roblox.com/v2/client-version/WindowsPlayer", "");
                if r.status != 200 {
                    return None;
                }
                hash_from_api(&r.body, &fv)
            })),
            ..VersionInputs::default()
        };
        version_candidates(&inp)
    }

    fn load(p: &RobloxProcess) -> Option<OffsetResult> {
        let cands = real_candidates(p);
        assert!(!cands.is_empty());
        let mut store = OffsetStore::new(app_data_dir().join("offsets").join("imtheo"), http(), required_keys());
        let r = store.load(&cands.first()?.hash);
        assert!(r.status == OffsetStatus::Ready, "{}", r.message);
        Some(r)
    }

    #[test]
    #[ignore]
    fn live_attach_and_datamodel() {
        let Some(p) = find() else { return };
        assert!(!p.file_version().is_empty());
        assert!(p.alive());
        let Some(r) = load(&p) else { return };
        let o = r.offsets.unwrap();
        let dm = data_model(&p, &o);
        assert_ne!(dm, 0);
        assert_eq!(instance_class(&p, &o, dm).unwrap_or_default(), "DataModel");
        println!("  base=0x{:x} dm=0x{dm:x}", p.base());
    }

    #[test]
    #[ignore]
    fn live_version_static_dir() {
        let Some(p) = find() else { return };
        let parts: Vec<&str> = p.file_version().split('.').collect();
        assert!(
            parts.len() == 4
                && parts
                    .iter()
                    .all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        );
        assert_eq!(p.file_version(), file_version_of(p.exe_path()).unwrap_or_default());
        let cands = real_candidates(&p);
        assert!(!cands.is_empty());
        let mut store = OffsetStore::new(app_data_dir().join("offsets").join("imtheo"), http(), required_keys());
        assert!(store.load(&cands[0].hash).status == OffsetStatus::Ready);
        println!(
            "  version={} first={} ({})",
            p.file_version(),
            cands[0].hash,
            cands[0].source
        );
    }

    #[test]
    #[ignore]
    fn live_shiftlock_sample() {
        let Some(p) = find() else { return };
        let Some(r) = load(&p) else { return };
        let o = r.offsets.unwrap();
        let s = sample_camera(&p, &o, data_model(&p, &o));
        let Some(s) = s else {
            panic!("no sample (not in a game / not on a humanoid?)");
        };
        assert!(s.distance > 0.0);
        println!("  rightOffset={:.3} distance={:.3}", s.right_offset, s.distance);
    }

    #[test]
    #[ignore]
    fn live_reader_sample() {
        let Some(p) = find() else { return };
        let Some(r) = load(&p) else { return };
        let o = r.offsets.unwrap();
        let guess = image_color_guess(&o);
        let mut rd = GameReader::new(p.clone(), o.clone(), Some(guess));
        let Some(s) = rd.sample() else {
            panic!("no sample (PlayerGui missing)");
        };
        println!(
            "  footing={:?} cooldown={:?} ult='{}' pct={:?}",
            s.footing, s.cooldown, s.ult_name, s.ult_percent
        );
        assert_ne!(s.footing, Footing::Unknown);
        assert!(!s.ult_name.is_empty());
        assert!(s.cooldown.is_some()); // the guess must read a valid triple
        let k = rd.read_keybinds();
        assert!(k.is_some());
        if let Some(k) = k {
            println!(
                "  keybinds jumpset={} ultimate={} spike={}",
                k.jumpset, k.ultimate, k.spike
            );
        }
        // Guess triple of the first Ground icon (Bump).
        let m: &dyn Memory = &p;
        let lp =
            m.get::<usize>(child_by_class(m, &o, data_model(&p, &o), "Players") + o["Player.LocalPlayer"] as usize);
        let gui = child_by_class(m, &o, lp.unwrap_or(0), "PlayerGui");
        let bump = resolve_path(
            m,
            &o,
            gui,
            &["Interface", "Game", "InGameActionsBar", "Actions", "Bump", "Icon"],
        )
        .inst;
        let g = if bump != 0 {
            m.get::<[f32; 3]>(bump + guess as usize)
        } else {
            None
        };
        println!("  imageColorGuess=0x{guess:x} Bump.Icon triple={g:?}");
    }

    #[test]
    #[ignore]
    fn live_updater_reaches_ready() {
        if find().is_none() {
            return;
        }
        // Hooks get a scratch dir: their keybind sync saves config.ini, which must not replace the user's settings.
        // The real calibration.ini is copied in so a known icon-color offset still validates without a cooldown cycle.
        let hook_dir = std::env::temp_dir().join("vbl_live_hooks");
        let _ = std::fs::remove_dir_all(&hook_dir);
        let _ = std::fs::create_dir_all(&hook_dir);
        let _ = std::fs::copy(app_data_dir().join("calibration.ini"), hook_dir.join("calibration.ini"));
        let u = Arc::new(
            Updater::new(http(), production_smoke_test, app_data_dir(), required_keys()).with_hooks(updater_hooks(
                Arc::new(SettingsStore::new(Settings::default())),
                &hook_dir,
            )),
        );
        {
            let t = {
                let u = u.clone();
                StopThread::spawn(move |st| u.run(st))
            };
            for _ in 0..150 {
                if u.status() == AppStatus::Ready {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(u.status(), AppStatus::Ready);
            let s = u.session();
            assert!(s.as_ref().is_some_and(|s| !s.offsets.values.is_empty()));
            assert!(s.as_ref().is_some_and(|s| icon_color(s).is_some()));
            println!(
                "  status={:?} detail='{}' iconColor=0x{:x}",
                u.status(),
                u.detail(),
                s.and_then(|s| icon_color(&s)).unwrap_or(0)
            );
            drop(t); // requests stop and joins
        }
        let _ = std::fs::remove_dir_all(hook_dir);
    }

    #[test]
    #[ignore]
    fn live_updater_unknown_version() {
        let dir = std::env::temp_dir().join("vbl_live_unknown");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = OffsetStore::new(&dir, http(), required_keys());
        let r = store.load("version-0000000000000000");
        assert!(r.status == OffsetStatus::WaitingForOffsets);
        assert_eq!(r.retry_in, Duration::from_secs(30));
        let _ = std::fs::remove_dir_all(dir);
    }
}
