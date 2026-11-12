use std::collections::HashMap;
use std::time::{Duration, Instant};

use handy_sdk::roblox::instance::{
    child_by_class, child_by_name, child_count, children, data_model, instance_class, instance_name, resolve_path,
};
use handy_sdk::roblox::memory::Memory;
use handy_sdk::roblox::offsets::Offsets;
use handy_sdk::roblox::process::RobloxProcess;
use handy_sdk::roblox::read::{
    VisibleProbe, character_of, gui_text, local_player, parent_of, part_position, part_rotation, part_size, player_gui,
    primary_part, primitive_of, workspace_of,
};
use handy_sdk::roblox::screen::Vec3;

use super::ball::{BallInfo, BallTracker, pick_ball, thin_axis};
use super::calibration::ColorCalibrator;
use super::shiftlock::sample_camera;
use super::state::{Footing, GameState, classify_footing, parse_percent};

/// Classes that are GuiObjects, so they have a Visible flag.
const GUI_CLASSES: [&str; 9] = [
    "Frame",
    "ScrollingFrame",
    "CanvasGroup",
    "ImageLabel",
    "ImageButton",
    "TextLabel",
    "TextButton",
    "TextBox",
    "ViewportFrame",
];
const BTN: [&str; 6] = ["Bump", "Set", "Dive", "Spike", "Block", "JumpSet"];
const READY_MIN: f32 = 0.9;
const CALIB_WINDOW: usize = 0x800; // bytes snapshotted per Icon, starting at Image-0x200

fn in_unit(v: &[f32; 3]) -> bool {
    v.iter().all(|c| *c >= 0.0 && *c <= 1.0) // rejects NaN
}

/// Every triple finite, in [0,1], equal channels and not all-zero (zero-filled memory must not pass).
pub fn valid_color_triples(t: &[[f32; 3]; 6]) -> bool {
    t.iter()
        .all(|v| in_unit(v) && v[0] == v[1] && v[1] == v[2] && v[0] != 0.0)
}

/// The six buttons' Visible bytes (Bump,Set,Dive then Spike,Block,JumpSet) cannot come from the real flag: one is
/// neither 0 nor 1 (version-4ea8e85481e34f46 later read 2 at the dumped offset), or ground and air buttons are both
/// shown, which the game never does (it first read 1 for all six).
pub fn flags_impossible(f: &[Option<u8>; 6]) -> bool {
    let shown = |t: &[Option<u8>]| t.contains(&Some(1));
    f.iter().any(|v| v.is_some_and(|v| v > 1)) || (shown(&f[..3]) && shown(&f[3..]))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keybinds {
    pub jumpset: String,
    pub ultimate: String,
    pub spike: String,
}

#[derive(Clone, Default)]
struct Cache {
    dm: usize,
    gui: usize,
    actions: usize,
    ult_title: usize,
    ult_pct: usize,
    ult_parent: usize,
    btn: [usize; 6],    // Bump,Set,Dive,Spike,Block,JumpSet
    others: Vec<usize>, // the Game frame's and the action bar's other GUI objects: a sanity check for the flag
    icon: [usize; 6],
    ws: usize,
    chr: usize,
    hrp: usize,
    net: usize,
    ws_kids: usize,         // a ball spawning or despawning changes this
    balls: Vec<[usize; 2]>, // {CLIENT_BALL_ model, its MeshPart}: practice has several
    net_normal: Vec3,
}

/// Walks PlayerGui and Workspace for footing, cooldown, ultimate, ball and keybind labels, caching what it found.
pub struct GameReader {
    proc: RobloxProcess,
    o: Offsets,
    icon_off: Option<u32>,
    // Where the buttons' Visible flag really is, once the dumped GuiObject.Visible proved wrong (it was on
    // version-4ea8e85481e34f46: every button read visible, so the macro saw the ground forever and kept jumping).
    vis: VisibleProbe,
    vis_flip: bool,                     // which trio the next calibration assumes is the shown one
    others_ok: bool,                    // the flag gave a real flag on c.others when last checked
    others_checked_at: Option<Instant>, // checked at most twice a second, only while every button reads hidden
    c: Cache,
    ball_trackers: HashMap<usize, BallTracker>, // by ball model
    last_resolve: Option<Instant>,
    t0: Instant, // monotonic origin of the ball trackers' seconds
}

impl GameReader {
    pub fn new(proc: RobloxProcess, o: Offsets, icon_color_offset: Option<u32>) -> Self {
        Self {
            proc,
            o,
            icon_off: icon_color_offset,
            vis: VisibleProbe::default(),
            vis_flip: false,
            others_ok: true,
            others_checked_at: None,
            c: Cache::default(),
            ball_trackers: HashMap::new(),
            last_resolve: None,
            t0: Instant::now(),
        }
    }

    fn m(&self) -> &dyn Memory {
        &self.proc
    }

    fn ensure(&mut self) -> bool {
        if !self.cache_valid() {
            self.last_resolve = Some(Instant::now());
            if !self.resolve() {
                return false;
            }
        }
        true
    }

    /// Reads the triple at `off` in all six icons (resolved even when hidden); None when they cannot be resolved.
    pub fn valid_color_at(&mut self, off: u32) -> Option<bool> {
        if !self.ensure() {
            return None;
        }
        let mut t = [[0.0f32; 3]; 6];
        for (i, v) in t.iter_mut().enumerate() {
            if self.c.icon[i] == 0 {
                return None;
            }
            match self.m().get::<[f32; 3]>(self.c.icon[i] + off as usize) {
                Some(x) => *v = x,
                None => return Some(false),
            }
        }
        Some(valid_color_triples(&t))
    }

    fn resolve(&mut self) -> bool {
        let (m, o) = (&self.proc as &dyn Memory, &self.o);
        let mut c = Cache {
            dm: data_model(&self.proc, o),
            ..Cache::default()
        };
        if c.dm == 0 {
            return false;
        }
        let lp = local_player(m, o, child_by_class(m, o, c.dm, "Players"));
        c.gui = player_gui(m, o, lp);
        if c.gui == 0 {
            self.c = Cache::default();
            return false;
        }
        c.actions = resolve_path(m, o, c.gui, &["Interface", "Game", "InGameActionsBar", "Actions"]).inst;
        if c.actions != 0 {
            let game = parent_of(m, o, parent_of(m, o, c.actions));
            // Only GUI objects carry the flag (layout and constraint objects in the bar do not).
            c.others = children(m, o, game)
                .into_iter()
                .chain(children(m, o, c.actions))
                .filter(|&g| instance_class(m, o, g).is_some_and(|cls| GUI_CLASSES.contains(&cls.as_str())))
                .collect();
            for (i, name) in BTN.iter().enumerate() {
                c.btn[i] = child_by_name(m, o, c.actions, name);
                c.icon[i] = if c.btn[i] != 0 {
                    child_by_name(m, o, c.btn[i], "Icon")
                } else {
                    0
                };
            }
        }
        let charge = resolve_path(m, o, c.gui, &["Interface", "Game", "UltimateCharge", "Charge"]).inst;
        if charge != 0 {
            c.ult_parent = charge;
            c.ult_title = child_by_name(m, o, charge, "Title");
            c.ult_pct = child_by_name(m, o, charge, "PercentCharged");
        }
        c.ws = workspace_of(m, o, c.dm);
        c.chr = character_of(m, o, lp);
        c.hrp = primary_part(m, o, c.chr);
        if c.ws != 0 {
            c.ws_kids = child_count(m, o, c.ws);
            c.net = resolve_path(m, o, c.ws, &["Map", "BallCollideOnly", "Net"]).inst;
            for k in children(m, o, c.ws) {
                // CLIENT_BALL_<id>: the id changes per ball
                if instance_name(m, o, k).unwrap_or_default().starts_with("CLIENT_BALL_") {
                    let part = child_by_class(m, o, k, "MeshPart");
                    if part != 0 {
                        c.balls.push([k, part]);
                    }
                }
            }
        }
        if primitive_of(m, o, c.net) != 0 {
            match (part_rotation(m, o, c.net), part_size(m, o, c.net)) {
                (Some(rot), Some(size)) => c.net_normal = thin_axis(&rot, size),
                _ => c.net = 0,
            }
        }
        self.c = c;
        true
    }

    // Cached addresses can dangle after a UI rebuild/respawn: check each is still parented where it was found.
    fn cache_valid(&self) -> bool {
        let (m, o, c) = (self.m(), &self.o, &self.c);
        if c.gui == 0 || data_model(&self.proc, o) != c.dm {
            return false; // a new place/teleport replaces the DataModel
        }
        let parent = |a: usize| parent_of(m, o, a);
        for i in 0..6 {
            if c.btn[i] != 0 && (parent(c.btn[i]) != c.actions || (c.icon[i] != 0 && parent(c.icon[i]) != c.btn[i])) {
                return false;
            }
        }
        if c.ult_title != 0 && parent(c.ult_title) != c.ult_parent {
            return false;
        }
        if c.ult_pct != 0 && parent(c.ult_pct) != c.ult_parent {
            return false;
        }
        if c.hrp != 0 && parent(c.hrp) != c.chr {
            return false; // respawn
        }
        if c.ws != 0 && child_count(m, o, c.ws) != c.ws_kids {
            return false;
        }
        for [model, part] in &c.balls {
            if parent(*model) != c.ws || parent(*part) != *model {
                return false;
            }
        }
        // Something not found yet (lobby, loading, between rallies): retry at most twice a second.
        let missing = c.actions == 0
            || c.ult_title == 0
            || c.ult_pct == 0
            || c.btn[0] == 0
            || c.icon[0] == 0
            || c.balls.is_empty()
            || c.hrp == 0
            || c.net == 0;
        !(missing
            && self
                .last_resolve
                .is_none_or(|t| t.elapsed() > Duration::from_millis(500)))
    }

    fn read_flags(&self) -> [Option<u8>; 6] {
        std::array::from_fn(|i| self.vis.raw(self.m(), &self.o, self.c.btn[i]))
    }

    /// Which of the six buttons are visible. When the flags cannot be right (see `flags_impossible`), or every
    /// button reads hidden while the Game frame's other objects show the offset holds no real flag either, the
    /// offset is wrong: let the probe search for the right one (it rescans at most twice a second, each time assuming
    /// the other trio is the shown one) and report none visible until it is found, so the footing reads Unknown and
    /// the macro does nothing instead of acting on a false "on the ground".
    fn buttons_visible(&mut self) -> [bool; 6] {
        let flags = self.read_flags();
        let all_hidden = !flags.contains(&Some(1));
        if all_hidden
            && self
                .others_checked_at
                .is_none_or(|t| t.elapsed() > Duration::from_millis(500))
        {
            self.others_checked_at = Some(Instant::now());
            self.others_ok = self.vis.plausible(self.m(), &self.o, &self.c.others);
        }
        if !flags_impossible(&flags) && !(all_hidden && !self.others_ok) {
            return flags.map(|v| v == Some(1));
        }
        let (ground, air) = (self.c.btn[..3].to_vec(), self.c.btn[3..].to_vec());
        let (shown, hidden) = if self.vis_flip { (air, ground) } else { (ground, air) };
        if self.vis.calibrate(&self.proc, &self.o, &shown, &hidden, &self.c.others) {
            self.vis_flip = !self.vis_flip;
        }
        let flags = self.read_flags();
        if flags_impossible(&flags) {
            [false; 6]
        } else {
            flags.map(|v| v == Some(1))
        }
    }

    /// None when the LocalPlayer/PlayerGui is unavailable. typing is left false (the caller reads chat focus).
    pub fn sample(&mut self) -> Option<GameState> {
        if !self.ensure() {
            return None;
        }
        let vis = self.buttons_visible();
        let (m, o, c) = (self.m(), &self.o, &self.c);
        let mut s = GameState {
            footing: classify_footing(&[vis[0], vis[1], vis[2]], &[vis[3], vis[4], vis[5]]),
            ..GameState::default()
        };
        if let Some(off) = self.icon_off
            && s.footing != Footing::Unknown
        {
            let base = if s.footing == Footing::Ground { 0 } else { 3 };
            let (mut any, mut ready) = (false, true);
            #[allow(clippy::needless_range_loop)] // indexes vis and c.icon together
            for i in base..base + 3 {
                if !vis[i] || c.icon[i] == 0 {
                    continue;
                }
                match m.get::<[f32; 3]>(c.icon[i] + off as usize).filter(in_unit) {
                    None => {
                        any = false;
                        break;
                    }
                    Some(rgb) => {
                        any = true;
                        ready = ready && rgb[0] >= READY_MIN;
                    }
                }
            }
            if any {
                s.cooldown = Some(!ready);
            }
        }
        if c.ult_title != 0 {
            s.ult_name = gui_text(m, o, c.ult_title).unwrap_or_default();
        }
        if let Some(t) = gui_text(m, o, c.ult_pct) {
            s.ult_percent = parse_percent(&t);
        }
        s.bar_found = c.actions != 0;
        s.cam = sample_camera(m, o, c.dm);
        s.ball = self.read_ball();
        Some(s)
    }

    fn read_ball(&mut self) -> Option<BallInfo> {
        let (m, o, c) = (&self.proc as &dyn Memory, &self.o, &self.c);
        let me = part_position(m, o, c.hrp);
        let net = part_position(m, o, c.net);
        self.ball_trackers.retain(|k, _| c.balls.iter().any(|b| b[0] == *k));
        let (me, net) = (me?, net?);
        let t = self.t0.elapsed().as_secs_f64();
        let mut seen = Vec::new();
        for [model, part] in &c.balls {
            if let Some(pos) = part_position(m, o, *part)
                && let Some(b) =
                    self.ball_trackers
                        .entry(*model)
                        .or_default()
                        .step(*model, me, pos, net, c.net_normal, t)
            {
                seen.push(b);
            }
        }
        pick_ball(&seen)
    }

    pub fn read_keybinds(&mut self) -> Option<Keybinds> {
        if !self.ensure() {
            return None;
        }
        let (m, o) = (self.m(), &self.o);
        let mut out = [String::new(), String::new(), String::new()];
        for (dst, name) in out.iter_mut().zip(["B_JumpSet", "B_Ultimate", "B_Spike"]) {
            let path = [
                "Interface",
                "Lobby",
                "Settings",
                "BGFrame",
                "Body",
                "List",
                name,
                "ImageButton",
                "KeyCode",
            ];
            let r = resolve_path(m, o, self.c.gui, &path);
            if r.inst == 0 {
                return None;
            }
            *dst = gui_text(m, o, r.inst)?;
        }
        let [jumpset, ultimate, spike] = out;
        Some(Keybinds {
            jumpset,
            ultimate,
            spike,
        })
    }

    /// true when locked
    pub fn feed_calibration(&mut self, cal: &mut ColorCalibrator) -> bool {
        if !self.ensure() {
            return false;
        }
        let lo = (self.o["GuiObject.Image"] - 0x200) as usize;
        let mut b: [Vec<u8>; 6] = Default::default();
        for (i, v) in b.iter_mut().enumerate() {
            if self.c.icon[i] == 0 {
                return false;
            }
            v.resize(CALIB_WINDOW, 0);
            if !self.m().read_slice(self.c.icon[i] + lo, v) {
                return false;
            }
        }
        cal.add_snapshot(&b, lo);
        cal.locked().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_flags_impossible_catches_both_wrong_offsets() {
        let f = |v: [u8; 6]| v.map(Some);
        assert!(!flags_impossible(&f([1, 1, 1, 0, 0, 0]))); // on the ground
        assert!(!flags_impossible(&f([0, 0, 0, 1, 1, 1]))); // in the air
        assert!(!flags_impossible(&f([0; 6]))); // action bar hidden: a real state
        assert!(flags_impossible(&f([1; 6]))); // first wrong offset: everything shown
        assert!(flags_impossible(&f([2; 6]))); // second wrong offset: not a flag at all
        assert!(flags_impossible(&f([1, 1, 1, 0, 127, 0])));
        assert!(!flags_impossible(&[None; 6])); // unreadable: nothing to judge
    }

    #[test]
    fn updater_color_triples_validation() {
        let ok = [[1.0f32; 3]; 6];
        assert!(valid_color_triples(&ok));
        let dim = [[0.5f32; 3]; 6];
        assert!(valid_color_triples(&dim));
        let zero = [[0.0f32; 3]; 6];
        assert!(!valid_color_triples(&zero));
        let mut nan = ok;
        nan[3][1] = f32::NAN;
        assert!(!valid_color_triples(&nan));
        let mut uneq = ok;
        uneq[2] = [1.0, 0.9, 1.0];
        assert!(!valid_color_triples(&uneq));
        let mut big = ok;
        big[0] = [2.0, 2.0, 2.0];
        assert!(!valid_color_triples(&big));
    }
}
