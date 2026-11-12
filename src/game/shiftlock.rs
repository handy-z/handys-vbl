use handy_sdk::roblox::instance::instance_class;
use handy_sdk::roblox::memory::Memory;
use handy_sdk::roblox::offsets::Offsets;
use handy_sdk::roblox::read::{
    camera_position, camera_rotation, camera_subject, current_camera, parent_of, part_position, primary_part,
};

pub const SHIFTLOCK_THRESHOLD: f32 = 0.875; // half of the default 1.75 shiftlock offset
pub const FIRST_PERSON_DIST: f32 = 2.0; // camera this close to the root part = first person

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraSample {
    pub right_offset: f32,
    pub distance: f32,
}

/// Third person: camera offset is the truth. First person (or no camera): carry state, flip on Shift.
#[derive(Default)]
pub struct ShiftlockTracker {
    state: Option<bool>,
}

impl ShiftlockTracker {
    pub fn step(&mut self, cam: Option<CameraSample>, shift_pressed_in_roblox: bool) -> Option<bool> {
        if let Some(c) = cam.filter(|c| c.distance >= FIRST_PERSON_DIST) {
            self.state = Some(c.right_offset > SHIFTLOCK_THRESHOLD);
        } else if shift_pressed_in_roblox && let Some(s) = self.state {
            self.state = Some(!s);
        }
        self.state
    }
}

pub fn sample_camera(m: &dyn Memory, o: &Offsets, dm: usize) -> Option<CameraSample> {
    let cam = current_camera(m, o, dm);
    if cam == 0 {
        return None;
    }
    let subj = camera_subject(m, o, cam);
    if subj == 0 || instance_class(m, o, subj).unwrap_or_default() != "Humanoid" {
        return None;
    }
    // Humanoid.HumanoidRootPart reads null: the root is the character model's PrimaryPart.
    let root = part_position(m, o, primary_part(m, o, parent_of(m, o, subj)))?;
    let pos = camera_position(m, o, cam)?;
    let r = camera_rotation(m, o, cam)?; // right = column 0 of the row-major 3x3
    let d = (pos.x - root.x, pos.y - root.y, pos.z - root.z);
    let off = d.0 * r[0] + d.1 * r[3] + d.2 * r[6];
    let dist = (d.0 * d.0 + d.1 * d.1 + d.2 * d.2).sqrt();
    if !off.is_finite() || !dist.is_finite() {
        return None;
    }
    Some(CameraSample {
        right_offset: off,
        distance: dist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(s: bool) -> ShiftlockTracker {
        let mut t = ShiftlockTracker::default();
        t.step(
            Some(CameraSample {
                right_offset: if s { 1.75 } else { 0.0 },
                distance: 12.0,
            }),
            false,
        );
        t
    }
    fn cam(right_offset: f32, distance: f32) -> Option<CameraSample> {
        Some(CameraSample { right_offset, distance })
    }

    #[test]
    fn shiftlock_third_person_from_camera() {
        let mut t = ShiftlockTracker::default();
        assert_eq!(t.step(cam(1.75, 12.0), false), Some(true));
        assert_eq!(t.step(cam(0.0, 12.0), false), Some(false));
    }

    #[test]
    fn shiftlock_first_person_flips_on_shift() {
        let mut t = with(false);
        assert_eq!(t.step(cam(0.0, 1.7), true), Some(true));
        assert_eq!(t.step(cam(0.0, 1.7), true), Some(false));
    }

    #[test]
    fn shiftlock_unknown_stays_unknown_in_first_person() {
        let mut t = ShiftlockTracker::default();
        assert!(t.step(cam(0.0, 1.7), true).is_none());
    }

    #[test]
    fn shiftlock_no_camera_keeps_state() {
        let mut t = with(true);
        assert_eq!(t.step(None, false), Some(true));
    }
}
