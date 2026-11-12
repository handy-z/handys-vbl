use std::collections::BTreeMap;

use handy_sdk::roblox::offsets::Offsets;

/// GuiObject.Image + 320
pub fn image_color_guess(o: &Offsets) -> u32 {
    (o["GuiObject.Image"] + 320) as u32
}

/// Finds the ImageColor3 field inside the Icon instance by watching all six action icons
/// go ready(1.0) -> cooldown(<1.0, all equal) -> ready(1.0) together.
#[derive(Default)]
pub struct ColorCalibrator {
    stage: BTreeMap<usize, i32>, // byte index -> 0 wait ready, 1 wait dim, 2 wait ready
    preferred: Option<u32>,      // wins when several fields complete the cycle together
    locked: Option<u32>,
}

impl ColorCalibrator {
    pub fn new(preferred: Option<u32>) -> Self {
        Self {
            preferred,
            ..Self::default()
        }
    }

    /// icon_bytes[i] = bytes [lo, lo+size) of icon i's instance.
    pub fn add_snapshot(&mut self, b: &[Vec<u8>; 6], lo: usize) {
        if self.locked.is_some() {
            return;
        }
        let n = b.iter().map(Vec::len).min().unwrap_or(0);
        let mut done: Vec<u32> = Vec::new();
        let mut i = 0;
        while i + 12 <= n {
            let triple = |k: usize| -> [f32; 3] {
                std::array::from_fn(|c| f32::from_ne_bytes(b[k][i + 4 * c..i + 4 * c + 4].try_into().unwrap()))
            };
            let t: [[f32; 3]; 6] = std::array::from_fn(triple);
            let valid = t.iter().flatten().all(|c| c.is_finite() && *c >= 0.0 && *c <= 1.0);
            let equal = (0..6).all(|k| b[k][i..i + 12] == b[0][i..i + 12]);
            if !valid {
                self.stage.remove(&i);
                i += 4;
                continue;
            }
            if !equal {
                i += 4;
                continue; // icons not dimmed together yet: no evidence either way
            }
            let one = t[0][0] == 1.0 && t[0][1] == 1.0 && t[0][2] == 1.0;
            let dim = t[0][0] == t[0][1] && t[0][1] == t[0][2] && t[0][0] < 1.0; // grey, not just "not white"
            let s = self.stage.entry(i).or_insert(0);
            if *s == 0 && one {
                *s = 1;
            } else if *s == 1 && dim {
                *s = 2;
            } else if *s == 2 && one {
                done.push((lo + i) as u32);
            }
            i += 4;
        }
        if done.is_empty() {
            return;
        }
        if done.len() == 1 {
            self.locked = Some(done[0]);
        } else if let Some(p) = self.preferred.filter(|p| done.contains(p)) {
            self.locked = Some(p);
        }
        if self.locked.is_none() {
            for st in self.stage.values_mut() {
                if *st == 2 {
                    *st = 0; // restart; a later cycle may disambiguate
                }
            }
        }
    }

    /// Field offset within the Icon instance.
    pub fn locked(&self) -> Option<u32> {
        self.locked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Snap = [Vec<u8>; 6];
    fn put(v: &mut [u8], off: usize, x: f32) {
        for i in 0..3 {
            v[off + 4 * i..off + 4 * i + 4].copy_from_slice(&x.to_ne_bytes());
        }
    }
    fn snap(at40: f32, dim_count: usize) -> Snap {
        std::array::from_fn(|k| {
            let mut s = vec![0u8; 0x100];
            put(&mut s, 0x40, if k < dim_count { at40 } else { 1.0 });
            put(&mut s, 0x80, 1.0); // unrelated constant white
            s
        })
    }

    #[test]
    fn calibration_locks_on_shared_dim() {
        let mut c = ColorCalibrator::default();
        c.add_snapshot(&snap(1.0, 6), 0x100);
        c.add_snapshot(&snap(0.5, 6), 0x100);
        assert!(c.locked().is_none());
        c.add_snapshot(&snap(1.0, 6), 0x100);
        assert_eq!(c.locked().unwrap_or(0), 0x140);
    }

    #[test]
    fn calibration_ignores_unequal_dims() {
        let mut c = ColorCalibrator::default();
        c.add_snapshot(&snap(1.0, 6), 0x100);
        c.add_snapshot(&snap(0.5, 3), 0x100);
        c.add_snapshot(&snap(1.0, 6), 0x100);
        assert!(c.locked().is_none());
    }

    #[test]
    fn calibration_guess_from_image() {
        let o = Offsets::from_pairs(&[("GuiObject.Image", 2520)]);
        assert_eq!(image_color_guess(&o), 2840);
    }

    // Fields at 0x40 and (optionally) 0x80 take `v`; `const_at_3c` puts a constant 1.0 float just before 0x40.
    fn snap2(v: f32, second: bool, const_at_3c: bool) -> Snap {
        std::array::from_fn(|_| {
            let mut s = vec![0u8; 0x100];
            put(&mut s, 0x40, v);
            if second {
                put(&mut s, 0x80, v);
            }
            if const_at_3c {
                put(&mut s, 0x3c, 1.0); // writes 0x3c..0x47, 0x40 rewritten below
                put(&mut s, 0x40, v);
            }
            s
        })
    }
    fn cycle(c: &mut ColorCalibrator, second: bool, const_at_3c: bool) {
        c.add_snapshot(&snap2(1.0, second, const_at_3c), 0x100);
        c.add_snapshot(&snap2(0.5, second, const_at_3c), 0x100);
        c.add_snapshot(&snap2(1.0, second, const_at_3c), 0x100);
    }

    #[test]
    fn calibration_neighbouring_constant_does_not_lock_early() {
        let mut c = ColorCalibrator::default();
        cycle(&mut c, false, true); // constant 1.0 at 0x3c, real triple at 0x40: window 0x3c reads (1, v, v)
        assert_eq!(c.locked().unwrap_or(0), 0x140);
    }

    #[test]
    fn calibration_prefers_guess_among_candidates() {
        let mut c = ColorCalibrator::new(Some(0x180));
        cycle(&mut c, true, false);
        assert_eq!(c.locked().unwrap_or(0), 0x180);
        let mut amb = ColorCalibrator::default();
        cycle(&mut amb, true, false);
        assert!(amb.locked().is_none());
    }

    #[test]
    fn calibration_constant_field_never_locks() {
        let mut c = ColorCalibrator::default();
        for _ in 0..4 {
            c.add_snapshot(&snap(1.0, 0), 0x100);
        }
        assert!(c.locked().is_none());
    }
}
