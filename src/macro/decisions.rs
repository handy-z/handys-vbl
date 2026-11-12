use crate::game::ball::BallInfo;
use crate::game::state::{Footing, GameState};

#[derive(Clone, Debug, PartialEq)]
pub struct MacroView {
    pub ground: bool,
    pub air: bool,
    pub ready: bool,
    pub shiftlock: Option<bool>,
    pub ult_on: bool,
    pub ult_name: String,
    pub ult_percent: Option<f64>,
}

pub fn to_view(s: &GameState, shiftlock: Option<bool>, ult_on: bool) -> MacroView {
    MacroView {
        ground: s.footing == Footing::Ground,
        air: s.footing == Footing::Air,
        ready: s.cooldown != Some(true),
        shiftlock,
        ult_on,
        ult_name: s.ult_name.clone(),
        ult_percent: s.ult_percent,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    TapSpace,
    PressJumpset,
    ReleaseJumpset,
    TapShift,
    WaitShiftDelay,
    TapUltimate,
    WaitUltSpikeGap,
    TapSpike,
    TapEsc,
    TapR,
    TapEnter,
    WaitResetGap,
    /// engine holds the rest of the plan until footing is Air
    WaitAir,
    /// engine owes one Shift tap, sent once airborne (or on abort)
    ShiftBackOnAir,
}
pub type Plan = Vec<Op>;

pub const BEFORE_SPIKE_ULTIMATES: [&str; 4] = ["SHIELD BREAKER", "DIVINE STRENGTH", "CURVE SPIKE", "REDIRECTION JUMP"];
pub const GROUND_ULTIMATE: &str = "BOOM JUMP";

/// ult_on && name matches && exactly 100%
pub fn ult_ready(v: &MacroView, name: &str) -> bool {
    v.ult_on && v.ult_name == name && v.ult_percent == Some(100.0)
}

// Pure translations of the X1/X2 behaviour in README.md (Controls); empty plan = do nothing.

/// On ground: space (unless X2 held), wait for air, jumpset.
pub fn on_x1_down(v: &MacroView, x2_held: bool) -> Plan {
    if !v.ready {
        return vec![];
    }
    if v.ground {
        return if x2_held {
            vec![Op::WaitAir, Op::PressJumpset]
        } else {
            vec![Op::TapSpace, Op::WaitAir, Op::PressJumpset]
        };
    }
    if v.air {
        return vec![Op::PressJumpset];
    }
    vec![]
}

pub fn on_x1_up(jumpset_held: bool) -> Plan {
    if jumpset_held { vec![Op::ReleaseJumpset] } else { vec![] }
}

/// One iteration of the X2-held loop.
pub fn x2_ground_step(v: &MacroView) -> Plan {
    if !v.ground || !v.ready {
        return vec![];
    }
    let boom = ult_ready(v, GROUND_ULTIMATE);
    let jump = if boom { Op::TapUltimate } else { Op::TapSpace };
    if v.shiftlock == Some(true) {
        return vec![jump];
    }
    // off or unknown: net-zero toggle. Boom jump launches along the body's facing, so shift lock stays on until airborne.
    vec![
        Op::TapShift,
        Op::WaitShiftDelay,
        jump,
        if boom { Op::ShiftBackOnAir } else { Op::TapShift },
    ]
}

pub fn on_x2_up(v: &MacroView) -> Plan {
    if !v.air || !v.ready {
        return vec![];
    }
    if BEFORE_SPIKE_ULTIMATES.iter().any(|n| ult_ready(v, n)) {
        return vec![Op::TapUltimate, Op::WaitUltSpikeGap, Op::TapSpike];
    }
    vec![Op::TapSpike]
}

/// Airborne, ready, ball coming into my side within reach_studs (0 = off): a plain spike, no ultimate.
#[allow(clippy::neg_cmp_op_on_partial_ord)] // a NaN speed or distance must not fire
pub fn counter_step(v: &MacroView, b: &BallInfo, reach_studs: i32) -> Plan {
    if reach_studs <= 0 || !(b.toward_me > 0.0) || !(b.dist <= reach_studs as f32) {
        return vec![];
    }
    if !v.air || !v.ready {
        return vec![];
    }
    vec![Op::TapSpike] // never an ultimate on a counter
}

pub fn reset_plan() -> Plan {
    vec![Op::TapEsc, Op::WaitResetGap, Op::TapR, Op::WaitResetGap, Op::TapEnter]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_full(
        f: Footing,
        ready: bool,
        shiftlock: Option<bool>,
        ult_on: bool,
        name: &str,
        pct: Option<f64>,
    ) -> MacroView {
        let s = GameState {
            footing: f,
            cooldown: Some(!ready),
            ult_name: name.into(),
            ult_percent: pct,
            ..GameState::default()
        };
        to_view(&s, shiftlock, ult_on)
    }
    fn view(f: Footing, ready: bool) -> MacroView {
        view_full(f, ready, Some(true), false, "", None)
    }
    fn view_sl(f: Footing, ready: bool, shiftlock: Option<bool>) -> MacroView {
        view_full(f, ready, shiftlock, false, "", None)
    }
    fn ult(f: Footing, ult_on: bool, name: &str, pct: Option<f64>) -> MacroView {
        view_full(f, true, Some(true), ult_on, name, pct)
    }
    fn bi(dist: f32, toward_me: f32) -> BallInfo {
        BallInfo { dist, toward_me }
    }

    #[test]
    fn x1_ground_ready() {
        assert_eq!(
            on_x1_down(&view(Footing::Ground, true), false),
            vec![Op::TapSpace, Op::WaitAir, Op::PressJumpset]
        );
        assert_eq!(on_x1_down(&view(Footing::Air, true), false), vec![Op::PressJumpset]);
        assert!(on_x1_down(&view(Footing::Ground, false), false).is_empty());
        assert!(on_x1_down(&view(Footing::Air, false), false).is_empty());
        assert!(on_x1_down(&view(Footing::Unknown, true), false).is_empty());
    }

    #[test]
    fn x1_ground_while_x2_held_skips_space() {
        assert_eq!(
            on_x1_down(&view(Footing::Ground, true), true),
            vec![Op::WaitAir, Op::PressJumpset]
        );
        assert_eq!(on_x1_down(&view(Footing::Air, true), true), vec![Op::PressJumpset]);
        assert!(on_x1_down(&view(Footing::Ground, false), true).is_empty());
    }

    #[test]
    fn x1_ready_when_cooldown_unknown() {
        let s = GameState {
            footing: Footing::Ground, // cooldown None = calibration pending -> treated as ready
            ..GameState::default()
        };
        assert!(to_view(&s, None, false).ready);
    }

    #[test]
    fn x1_up_without_press_sends_nothing() {
        assert!(on_x1_up(false).is_empty());
        assert_eq!(on_x1_up(true), vec![Op::ReleaseJumpset]);
    }

    #[test]
    fn x2_ground_shiftlock_on() {
        assert_eq!(
            x2_ground_step(&view_sl(Footing::Ground, true, Some(true))),
            vec![Op::TapSpace]
        );
        assert_eq!(
            x2_ground_step(&view_full(
                Footing::Ground,
                true,
                Some(true),
                true,
                "BOOM JUMP",
                Some(100.0)
            )),
            vec![Op::TapUltimate]
        );
        assert_eq!(
            x2_ground_step(&view_full(
                Footing::Ground,
                true,
                Some(true),
                false,
                "BOOM JUMP",
                Some(100.0)
            )),
            vec![Op::TapSpace]
        );
    }

    #[test]
    fn x2_ground_shiftlock_off() {
        let jump = vec![Op::TapShift, Op::WaitShiftDelay, Op::TapSpace, Op::TapShift];
        // boom jump launches along the body's facing: keep shift lock on until airborne
        let boom = vec![Op::TapShift, Op::WaitShiftDelay, Op::TapUltimate, Op::ShiftBackOnAir];
        assert_eq!(x2_ground_step(&view_sl(Footing::Ground, true, Some(false))), jump);
        assert_eq!(
            x2_ground_step(&view_full(
                Footing::Ground,
                true,
                Some(false),
                true,
                "BOOM JUMP",
                Some(100.0)
            )),
            boom
        );
        assert_eq!(x2_ground_step(&view_sl(Footing::Ground, true, None)), jump); // unknown -> else branch
    }

    #[test]
    fn x2_ground_not_ready_or_air() {
        assert!(x2_ground_step(&view(Footing::Ground, false)).is_empty());
        assert!(x2_ground_step(&view(Footing::Air, true)).is_empty());
        assert!(x2_ground_step(&view(Footing::Unknown, true)).is_empty());
    }

    #[test]
    fn x2_up_air() {
        let ult_spike = vec![Op::TapUltimate, Op::WaitUltSpikeGap, Op::TapSpike];
        assert_eq!(
            on_x2_up(&ult(Footing::Air, true, "CURVE SPIKE", Some(100.0))),
            ult_spike
        );
        for n in BEFORE_SPIKE_ULTIMATES {
            assert_eq!(on_x2_up(&ult(Footing::Air, true, n, Some(100.0))), ult_spike);
        }
        assert_eq!(
            on_x2_up(&ult(Footing::Air, true, "LEAD FEET", Some(100.0))),
            vec![Op::TapSpike]
        );
        assert_eq!(
            on_x2_up(&ult(Footing::Air, true, "BOOM JUMP", Some(100.0))),
            vec![Op::TapSpike]
        );
        assert_eq!(
            on_x2_up(&ult(Footing::Air, false, "CURVE SPIKE", Some(100.0))),
            vec![Op::TapSpike]
        );
        assert!(on_x2_up(&ult(Footing::Ground, true, "CURVE SPIKE", Some(100.0))).is_empty());
        assert!(on_x2_up(&view(Footing::Air, false)).is_empty());
    }

    #[test]
    fn ultimate_requires_exactly_100() {
        assert_eq!(
            on_x2_up(&ult(Footing::Air, true, "SHIELD BREAKER", Some(99.5))),
            vec![Op::TapSpike]
        );
        assert_eq!(
            on_x2_up(&ult(Footing::Air, true, "SHIELD BREAKER", None)),
            vec![Op::TapSpike]
        );
        assert!(!ult_ready(
            &ult(Footing::Air, true, "BOOM JUMP", Some(99.9)),
            GROUND_ULTIMATE
        ));
        assert!(ult_ready(
            &ult(Footing::Air, true, "BOOM JUMP", Some(100.0)),
            GROUND_ULTIMATE
        ));
    }

    #[test]
    fn reset_plan() {
        assert_eq!(
            super::reset_plan(),
            vec![Op::TapEsc, Op::WaitResetGap, Op::TapR, Op::WaitResetGap, Op::TapEnter]
        );
    }

    #[test]
    fn counter_fires_in_reach_incoming() {
        assert_eq!(
            counter_step(&view(Footing::Air, true), &bi(5.0, 30.0), 6),
            vec![Op::TapSpike]
        );
        assert_eq!(
            counter_step(&view(Footing::Air, true), &bi(6.0, 30.0), 6),
            vec![Op::TapSpike]
        ); // reach is inclusive
    }

    #[test]
    fn counter_never_uses_ult() {
        assert_eq!(
            counter_step(
                &ult(Footing::Air, true, "SHIELD BREAKER", Some(100.0)),
                &bi(5.0, 30.0),
                6
            ),
            vec![Op::TapSpike]
        );
    }

    #[test]
    fn counter_skips() {
        assert!(counter_step(&view(Footing::Air, true), &bi(5.0, 30.0), 0).is_empty()); // off
        assert!(counter_step(&view(Footing::Air, true), &bi(7.0, 30.0), 6).is_empty()); // out of reach
        assert!(counter_step(&view(Footing::Air, true), &bi(5.0, 0.0), 6).is_empty()); // still ball
        assert!(counter_step(&view(Footing::Air, true), &bi(5.0, -30.0), 6).is_empty()); // going away
        assert!(counter_step(&view(Footing::Ground, true), &bi(5.0, 30.0), 6).is_empty());
        assert!(counter_step(&view(Footing::Air, false), &bi(5.0, 30.0), 6).is_empty()); // cooldown
    }

    #[test]
    fn counter_skips_nan_speed() {
        assert!(counter_step(&view(Footing::Air, true), &bi(5.0, f32::NAN), 6).is_empty());
    }
}
