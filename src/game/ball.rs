use std::collections::VecDeque;

use handy_sdk::roblox::read::Mat3;
use handy_sdk::roblox::screen::{Vec3, distance};

pub const BALL_WINDOW_SEC: f64 = 0.04; // the ball moves once per rendered frame: estimate over >= 40 ms

// Reach is measured to my body: a vertical line from the feet to the hands, relative to the root part
// (spikes connect up to ~10 studs above it and with balls passing at chest height; tools/debug/ball_debug.py).
pub const BODY_BOTTOM: f32 = -3.0;
pub const BODY_TOP: f32 = 7.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BallInfo {
    /// studs from my body line (root part + BODY_BOTTOM .. BODY_TOP)
    pub dist: f32,
    /// studs/s into my side of the net (> 0 = incoming)
    pub toward_me: f32,
}

#[derive(Clone, Copy)]
struct Sample {
    t: f64,
    p: Vec3,
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a.x * b.x + a.y * b.y + a.z * b.z
}
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    Vec3 {
        x: a.x - b.x,
        y: a.y - b.y,
        z: a.z - b.z,
    }
}

/// The ball is moved by a client script (its physics velocity reads 0), so speed comes from position history.
#[derive(Default)]
pub struct BallTracker {
    id: usize,
    hist: VecDeque<Sample>,
}

impl BallTracker {
    /// t_sec: monotonic seconds. None until a sample at least BALL_WINDOW_SEC old exists for this ball.
    pub fn step(&mut self, ball_id: usize, me: Vec3, ball: Vec3, net: Vec3, n: Vec3, t: f64) -> Option<BallInfo> {
        // A gap means the ball was unreadable or gone; a new ball can reuse the same address.
        if ball_id != self.id || self.hist.back().is_some_and(|b| t - b.t > 2.0 * BALL_WINDOW_SEC) {
            self.id = ball_id;
            self.hist.clear();
        }
        self.hist.push_back(Sample { t, p: ball });
        // Keep the newest sample that is at least a window old as the reference.
        while self.hist.len() > 1 && t - self.hist[1].t >= BALL_WINDOW_SEC {
            self.hist.pop_front();
        }
        let r = self.hist[0];
        let dt = t - r.t;
        if dt < BALL_WINDOW_SEC {
            return None;
        }
        let side = if dot(sub(me, net), n) >= 0.0 { 1.0 } else { -1.0 };
        let toward = dot(sub(ball, r.p), n) * side / dt as f32;
        // nearest point on my body line; std::clamp's comparisons, so a NaN never panics
        let (lo, hi) = (me.y + BODY_BOTTOM, me.y + BODY_TOP);
        let y = if ball.y < lo {
            lo
        } else if hi < ball.y {
            hi
        } else {
            ball.y
        };
        Some(BallInfo {
            dist: distance(Vec3 { x: me.x, y, z: me.z }, ball),
            toward_me: toward,
        })
    }
}

/// Nearest ball coming into my side, else the nearest ball; None when empty.
pub fn pick_ball(balls: &[BallInfo]) -> Option<BallInfo> {
    let mut best: Option<BallInfo> = None;
    for b in balls {
        let inc = b.toward_me > 0.0;
        let best_in = best.is_some_and(|x| x.toward_me > 0.0);
        if best.is_none_or(|x| (inc && !best_in) || (inc == best_in && b.dist < x.dist)) {
            best = Some(*b);
        }
    }
    best
}

/// Column of the smallest size axis of a row-major rotation.
pub fn thin_axis(r: &Mat3, s: Vec3) -> Vec3 {
    let i = if s.x <= s.y && s.x <= s.z {
        0
    } else if s.y <= s.z {
        1
    } else {
        2
    };
    Vec3 {
        x: r[i],
        y: r[3 + i],
        z: r[6 + i],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use handy_sdk::roblox::screen::vec3;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }
    const NET: Vec3 = vec3(0.0, 0.0, 0.0);
    const N: Vec3 = vec3(0.0, 0.0, 1.0);
    const ME: Vec3 = vec3(0.0, 0.0, -10.0);

    #[test]
    fn ball_tracker_needs_history() {
        let mut t = BallTracker::default();
        assert!(t.step(1, ME, vec3(0.0, 5.0, -2.0), NET, N, 0.0).is_none());
        let b = t.step(1, ME, vec3(0.0, 5.0, -2.0), NET, N, 0.05); // still ball (serve toss)
        assert!(b.is_some_and(|b| b.toward_me == 0.0));
    }

    #[test]
    fn ball_tracker_incoming_sign() {
        let mut inc = BallTracker::default();
        inc.step(1, ME, vec3(0.0, 0.0, -2.0), NET, N, 0.0);
        let b = inc.step(1, ME, vec3(0.0, 0.0, -4.0), NET, N, 0.05);
        assert!(b.is_some_and(|b| approx(b.toward_me, 40.0) && approx(b.dist, 6.0)));
        let mut out = BallTracker::default();
        out.step(1, ME, vec3(0.0, 0.0, -4.0), NET, N, 0.0);
        let b = out.step(1, ME, vec3(0.0, 0.0, -2.0), NET, N, 0.05);
        assert!(b.is_some_and(|b| approx(b.toward_me, -40.0)));
        let mut mirrored = BallTracker::default(); // my side is +z: incoming means moving toward +z
        mirrored.step(1, vec3(0.0, 0.0, 10.0), vec3(0.0, 0.0, 2.0), NET, N, 0.0);
        let b = mirrored.step(1, vec3(0.0, 0.0, 10.0), vec3(0.0, 0.0, 4.0), NET, N, 0.05);
        assert!(b.is_some_and(|b| approx(b.toward_me, 40.0)));
    }

    #[test]
    fn ball_tracker_window() {
        let mut t = BallTracker::default();
        // 6 ms steps at 100 studs/s toward me: no estimate before 40 ms, then never a gap.
        for i in 0..7 {
            assert!(
                t.step(1, ME, vec3(0.0, 0.0, -0.6 * i as f32), NET, N, 0.006 * f64::from(i))
                    .is_none()
            );
        }
        for i in 7..40 {
            let b = t.step(1, ME, vec3(0.0, 0.0, -0.6 * i as f32), NET, N, 0.006 * f64::from(i));
            assert!(b.is_some_and(|b| approx(b.toward_me, 100.0)));
        }
    }

    #[test]
    fn ball_tracker_resets_on_new_ball() {
        let mut t = BallTracker::default();
        t.step(1, ME, vec3(0.0, 0.0, -2.0), NET, N, 0.0);
        assert!(t.step(1, ME, vec3(0.0, 0.0, -4.0), NET, N, 0.05).is_some());
        assert!(t.step(2, ME, vec3(0.0, 0.0, -4.0), NET, N, 0.10).is_none());
    }

    #[test]
    fn ball_thin_axis() {
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let n = thin_axis(&id, vec3(52.0, 8.85, 1.0));
        assert!(n.x == 0.0 && n.y == 0.0 && n.z == 1.0);
        let yaw90 = [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0]; // 90 deg about y, row-major
        let n = thin_axis(&yaw90, vec3(52.0, 8.85, 1.0));
        assert!(n.x == 1.0 && n.y == 0.0 && n.z == 0.0);
    }

    #[test]
    fn ball_tracker_gap_resets() {
        let mut t = BallTracker::default(); // same id after a gap (heap address reused by the next ball): old samples must not count
        t.step(1, ME, vec3(0.0, 0.0, 40.0), NET, N, 0.0);
        assert!(t.step(1, ME, vec3(0.0, 0.0, -4.0), NET, N, 3.0).is_none());
    }

    #[test]
    fn ball_dist_to_body() {
        // Distance to my body: a vertical line from BODY_BOTTOM to BODY_TOP around the root part.
        let dist = |ball: Vec3| {
            let mut t = BallTracker::default();
            t.step(1, ME, ball, NET, N, 0.0);
            t.step(1, ME, ball, NET, N, 0.05).unwrap().dist
        };
        assert!(approx(dist(vec3(0.0, 0.0, -10.0)), 0.0)); // through my chest
        assert!(approx(dist(vec3(3.0, 0.0, -10.0)), 3.0)); // beside my chest
        assert!(approx(dist(vec3(0.0, BODY_TOP, -10.0)), 0.0)); // at my hands
        assert!(approx(dist(vec3(0.0, BODY_TOP + 2.0, -10.0)), 2.0)); // above my hands
        assert!(approx(dist(vec3(4.0, BODY_BOTTOM - 3.0, -10.0)), 5.0)); // beside and below my feet
    }

    #[test]
    fn ball_pick_nearest_incoming() {
        let bi = |dist, toward_me| BallInfo { dist, toward_me };
        assert!(pick_ball(&[]).is_none());
        // practice servers have several balls: the nearest one coming at me wins over a nearer one going away
        let b = pick_ball(&[bi(3.0, -20.0), bi(9.0, 30.0), bi(5.0, 10.0)]);
        assert!(b.is_some_and(|b| b.dist == 5.0 && b.toward_me == 10.0));
        let b = pick_ball(&[bi(8.0, -5.0), bi(4.0, 0.0)]); // nothing incoming: nearest
        assert!(b.is_some_and(|b| b.dist == 4.0));
    }
}
