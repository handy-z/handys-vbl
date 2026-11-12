use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use handy_sdk::input::hooks::HotkeyEdge;
use handy_sdk::util::spsc_queue::SpscQueue;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LSHIFT, VK_RSHIFT, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    X1Down,
    X1Up,
    X2Down,
    X2Up,
    ResetKey,
    ToggleUltKey,
    ShiftDown,
}

#[derive(Clone, Copy, Debug)]
pub struct InputEvent {
    pub kind: EventType,
    pub qpc: i64,
    pub injected: bool,
}

/// Hook thread -> engine thread; a full queue drops the event.
pub type InputQueue = SpscQueue<InputEvent, 256>;

/// X1/X2 are swallowed only while Roblox is foreground; hotkeys and Shift always pass through.
pub fn should_swallow(t: EventType, roblox_foreground: bool) -> bool {
    match t {
        EventType::X1Down | EventType::X1Up | EventType::X2Down | EventType::X2Up => roblox_foreground,
        _ => false,
    }
}

/// Pairs each X up with its down: an up is swallowed iff its down was, regardless of focus.
/// Injected events are never swallowed and leave the state untouched. Hook thread only (atomics, so the hook
/// closure can hold it).
#[derive(Default)]
pub struct XSwallowState {
    swallowed: [AtomicBool; 2],
}

impl XSwallowState {
    pub fn decide(&self, t: EventType, roblox_foreground: bool, injected: bool) -> bool {
        if injected {
            return false;
        }
        match t {
            EventType::X1Down => {
                let v = should_swallow(t, roblox_foreground);
                self.swallowed[0].store(v, Relaxed);
                v
            }
            EventType::X2Down => {
                let v = should_swallow(t, roblox_foreground);
                self.swallowed[1].store(v, Relaxed);
                v
            }
            EventType::X1Up => self.swallowed[0].swap(false, Relaxed),
            EventType::X2Up => self.swallowed[1].swap(false, Relaxed),
            _ => false,
        }
    }
}

// Pure classifiers (unit-tested). classify_key tracks down state per hotkey to drop auto-repeat (HotkeyEdge:
// a repeat >1100 ms after the last down/repeat counts as a fresh press, recovering a missed key-up).
// With Alt/Ctrl/Win down (modifier_down) a hotkey is tracked but never fires, so Alt+F4 leaves the ultimate alone.

pub fn classify_mouse(msg: u32, mouse_data: u32) -> Option<EventType> {
    if msg != WM_XBUTTONDOWN && msg != WM_XBUTTONUP {
        return None;
    }
    let down = msg == WM_XBUTTONDOWN;
    match (mouse_data >> 16) as u16 {
        XBUTTON1 => Some(if down { EventType::X1Down } else { EventType::X1Up }),
        XBUTTON2 => Some(if down { EventType::X2Down } else { EventType::X2Up }),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)] // one flag per input the rule depends on
pub fn classify_key(
    msg: u32,
    vk: u32,
    reset_vk: u32,
    toggle_vk: u32,
    time_ms: u32,
    reset: &mut HotkeyEdge,
    toggle: &mut HotkeyEdge,
    modifier_down: bool,
) -> Option<EventType> {
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    if !down && !up {
        return None;
    }
    if vk == reset_vk {
        return (reset.fresh(down, time_ms) && !modifier_down).then_some(EventType::ResetKey);
    }
    if vk == toggle_vk {
        return (toggle.fresh(down, time_ms) && !modifier_down).then_some(EventType::ToggleUltKey);
    }
    if down && [VK_LSHIFT, VK_RSHIFT, VK_SHIFT].iter().any(|k| u32::from(k.0) == vk) {
        return Some(EventType::ShiftDown);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F1, VK_F4};
    use windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN;

    const F1: u32 = VK_F1.0 as u32;
    const F4: u32 = VK_F4.0 as u32;
    const LSHIFT: u32 = VK_LSHIFT.0 as u32;
    const X1: u32 = XBUTTON1 as u32;
    const X2: u32 = XBUTTON2 as u32;

    #[test]
    fn hooks_passthrough_when_not_foreground() {
        assert!(!should_swallow(EventType::X1Down, false));
        assert!(should_swallow(EventType::X2Up, true));
        assert!(!should_swallow(EventType::ResetKey, true));
        assert!(!should_swallow(EventType::ShiftDown, true));
    }

    #[test]
    fn hooks_classify_mouse() {
        assert_eq!(classify_mouse(WM_XBUTTONDOWN, X1 << 16), Some(EventType::X1Down));
        assert_eq!(classify_mouse(WM_XBUTTONUP, X2 << 16), Some(EventType::X2Up));
        assert!(classify_mouse(WM_LBUTTONDOWN, 0).is_none());
        assert!(classify_mouse(WM_XBUTTONDOWN, 3 << 16).is_none());
    }

    #[test]
    fn hooks_classify_key_ignores_repeat() {
        let (mut rd, mut td) = (HotkeyEdge::default(), HotkeyEdge::default());
        let mut k = |msg, vk, t| classify_key(msg, vk, F1, F4, t, &mut rd, &mut td, false);
        assert_eq!(k(WM_KEYDOWN, F1, 1000), Some(EventType::ResetKey));
        assert!(k(WM_KEYDOWN, F1, 1500).is_none()); // auto-repeat
        assert!(k(WM_KEYUP, F1, 1600).is_none());
        assert_eq!(k(WM_SYSKEYDOWN, F1, 1700), Some(EventType::ResetKey));
        assert_eq!(k(WM_KEYDOWN, F4, 1700), Some(EventType::ToggleUltKey));
        assert_eq!(k(WM_KEYDOWN, LSHIFT, 1700), Some(EventType::ShiftDown));
        assert!(k(WM_KEYDOWN, u32::from(b'A'), 1700).is_none());
    }

    #[test]
    fn hooks_classify_key_ignores_modified_hotkeys() {
        let (mut rd, mut td) = (HotkeyEdge::default(), HotkeyEdge::default());
        let mut k = |msg, vk, t, m| classify_key(msg, vk, F1, F4, t, &mut rd, &mut td, m);
        // Alt+F4 / Ctrl+F1: down state is still tracked, but nothing fires
        assert!(k(WM_SYSKEYDOWN, F4, 1000, true).is_none());
        assert!(k(WM_SYSKEYDOWN, F4, 1500, false).is_none()); // repeat after Alt up
        assert!(k(WM_SYSKEYUP, F4, 1600, true).is_none());
        assert_eq!(k(WM_KEYDOWN, F4, 1700, false), Some(EventType::ToggleUltKey));
        assert!(k(WM_KEYDOWN, F1, 1700, true).is_none());
        assert_eq!(k(WM_KEYDOWN, LSHIFT, 1700, true), Some(EventType::ShiftDown));
    }

    #[test]
    fn hooks_classify_key_recovers_missed_keyup() {
        let (mut rd, mut td) = (HotkeyEdge::default(), HotkeyEdge::default());
        let mut k = |vk, t| classify_key(WM_KEYDOWN, vk, F1, F4, t, &mut rd, &mut td, false);
        assert_eq!(k(F4, 1000), Some(EventType::ToggleUltKey));
        // held: 1000 ms initial delay then 33 ms repeats are never fresh, however long the hold
        assert!(k(F4, 2000).is_none());
        for t in (2033..5000).step_by(33) {
            assert!(k(F4, t).is_none());
        }
        // last repeat was at 4970; key-up missed. Exactly 1100 ms later is still a repeat, beyond that is fresh.
        assert!(k(F4, 6070).is_none());
        assert_eq!(k(F4, 7195), Some(EventType::ToggleUltKey));
        // tick wrap-around
        let (mut r2, mut t2) = (HotkeyEdge::default(), HotkeyEdge::default());
        let mut k2 = |t| classify_key(WM_KEYDOWN, F1, F1, F4, t, &mut r2, &mut t2, false);
        assert_eq!(k2(0xFFFF_FF00), Some(EventType::ResetKey));
        assert!(k2(0x100).is_none()); // 512 ms later
        assert_eq!(k2(0x700), Some(EventType::ResetKey)); // 1536 ms
    }

    #[test]
    fn hooks_xswallow_pairs_up_with_down() {
        let s = XSwallowState::default();
        assert!(s.decide(EventType::X1Down, true, false));
        assert!(s.decide(EventType::X1Up, false, false)); // down swallowed -> up swallowed
        assert!(!s.decide(EventType::X1Up, true, false)); // flag cleared
        assert!(!s.decide(EventType::X1Down, false, false));
        assert!(!s.decide(EventType::X1Up, true, false)); // down passed -> up passed
    }

    #[test]
    fn hooks_xswallow_buttons_independent_and_injected_ignored() {
        let s = XSwallowState::default();
        assert!(s.decide(EventType::X1Down, true, false));
        assert!(!s.decide(EventType::X2Up, true, false));
        assert!(s.decide(EventType::X1Up, false, false));
        assert!(s.decide(EventType::X2Down, true, false));
        assert!(!s.decide(EventType::X2Down, true, true)); // injected never swallowed
        assert!(!s.decide(EventType::X2Up, true, true)); // and doesn't clear state
        assert!(s.decide(EventType::X2Up, false, false));
        assert!(!s.decide(EventType::ResetKey, true, false));
    }
}
