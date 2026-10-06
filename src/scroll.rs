//! Intercept the first wheel event and stable gaze switches to another window,
//! and replay that exact wheel delta. Raw input alone arrives too late to reroute
//! the first event. The hook never waits for locks, renders, or accesses files.
//! Driver-generated wheel events are valid input: filter only our tagged replays,
//! not all LLMHF_INJECTED events (which can include trackpad helper software).
use std::{
    cell::RefCell,
    mem::{size_of, zeroed},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::*,
    System::SystemInformation::GetTickCount,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
pub const SCROLLED: u32 = WM_APP + 2;
const REPLAY_TAG: usize = 0x45594553;
#[derive(Default)]
struct Gesture {
    last: Option<u32>,
}
impl Gesture {
    fn first(&mut self, now: u32) -> bool {
        let first = self.last.is_none_or(|t| now.wrapping_sub(t) >= 500);
        self.last = Some(now);
        first
    }
}
struct State {
    owner: HWND,
    enabled: bool,
    point: Option<[i32; 2]>,
    updated: u32,
    gesture: Gesture,
    focus: FocusLatch,
    count: u64,
    received: u64,
    synthesized: u64,
    status: &'static str,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
pub struct ScrollFocus(HHOOK);
impl ScrollFocus {
    pub unsafe fn start(owner: HWND) -> Result<Self, String> {
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(callback), null_mut(), 0);
        if hook.is_null() {
            return Err("Could not install gaze scroll hook".into());
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                owner,
                enabled: false,
                point: None,
                updated: 0,
                gesture: Gesture::default(),
                focus: FocusLatch::default(),
                count: 0,
                received: 0,
                synthesized: 0,
                status: "Ready",
            })
        });
        Ok(Self(hook))
    }
}
impl Drop for ScrollFocus {
    fn drop(&mut self) {
        unsafe {
            UnhookWindowsHookEx(self.0);
        }
        STATE.with(|s| s.borrow_mut().take());
    }
}
pub fn update(enabled: bool, point: Option<[i32; 2]>, now: u32) {
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            if enabled != s.enabled {
                s.gesture = Gesture::default();
                s.focus = FocusLatch::default();
            }
            s.enabled = enabled;
            s.point = point;
            s.updated = now;
        }
    });
}
pub fn status() -> String {
    STATE.with(|s| {
        s.borrow().as_ref().map_or_else(
            || "Scroll OFF".into(),
            |s| {
                format!(
                    "Scroll: {} events ({} synthetic), {} targets\n{}",
                    s.received, s.synthesized, s.count, s.status
                )
            },
        )
    })
}
/// Native Precision Touchpads can scroll through Direct Manipulation without
/// emitting wheel events. Focus from their two-finger HID gesture instead.
pub unsafe fn touchpad(active: bool, trigger: bool, scrolling: bool) {
    let now = GetTickCount();
    let point = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut()?;
        if !s.enabled {
            return None;
        }
        if active {
            s.gesture.last = Some(now);
        }
        if !scrolling {
            return None;
        }
        if s.point.is_none() || now.wrapping_sub(s.updated) > 100 {
            s.status = "Touchpad scroll: no fresh gaze";
            return None;
        }
        s.point
    });
    let Some([x, y]) = point else {
        return;
    };
    let root = GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT);
    if root.is_null()
        || IsWindowEnabled(root) == 0
        || GetWindowLongW(root, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE != 0
    {
        set_status("Touchpad: no activatable window under gaze");
        return;
    }
    if !choose_target(root, now, trigger) {
        return;
    }
    if SetCursorPos(x, y) == 0 {
        set_status("Touchpad: pointer movement failed");
        return;
    }
    let focused = GetForegroundWindow() == root || SetForegroundWindow(root) != 0;
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.count += 1;
            s.status = if focused {
                "Touchpad: focused gaze window"
            } else {
                "Touchpad: pointer moved; focus denied"
            };
        }
    });
}
fn wheel_input(message: u32, data: u32) -> INPUT {
    let delta = (data >> 16) as i16 as i32;
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                mouseData: delta as u32,
                dwFlags: if message == WM_MOUSEHWHEEL {
                    MOUSEEVENTF_HWHEEL
                } else {
                    MOUSEEVENTF_WHEEL
                },
                dwExtraInfo: REPLAY_TAG,
                ..unsafe { zeroed() }
            },
        },
    }
}
fn accept_wheel(event: &MSLLHOOKSTRUCT) -> bool {
    event.dwExtraInfo != REPLAY_TAG && event.mouseData >> 16 != 0
}
unsafe extern "system" fn callback(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code < 0 || !matches!(w as u32, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        return CallNextHookEx(null_mut(), code, w, l);
    }
    let event = &*(l as *const MSLLHOOKSTRUCT);
    if !accept_wheel(event) {
        return CallNextHookEx(null_mut(), code, w, l);
    }
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.received += 1;
            s.synthesized += u64::from(event.flags & LLMHF_INJECTED != 0);
        }
    });
    // Leave drag, zoom, and modifier gestures alone.
    let blocked = [
        VK_LBUTTON,
        VK_RBUTTON,
        VK_MBUTTON,
        VK_XBUTTON1,
        VK_XBUTTON2,
        VK_CONTROL,
        VK_MENU,
        VK_SHIFT,
    ]
    .iter()
    .any(|k| GetAsyncKeyState(*k as i32) < 0);
    let now = GetTickCount();
    let action = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut()?;
        if !s.enabled || blocked {
            s.status = if !s.enabled {
                "Scroll assist paused"
            } else {
                "Normal scroll: modifier or button held"
            };
            return None;
        }
        PostMessageW(s.owner, SCROLLED, now as WPARAM, 0);
        let first = s.gesture.first(now);
        if now.wrapping_sub(s.updated) > 100 || s.point.is_none() {
            s.status = "No fresh gaze; normal scrolling";
            return None;
        }
        s.point.map(|point| (point, first))
    });
    let Some(([x, y], first)) = action else {
        return CallNextHookEx(null_mut(), code, w, l);
    };
    let root = GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT);
    if root.is_null()
        || IsWindowEnabled(root) == 0
        || GetWindowLongW(root, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE != 0
    {
        set_status("No activatable window under gaze");
        return CallNextHookEx(null_mut(), code, w, l);
    }
    let mut old: POINT = zeroed();
    if !choose_target(root, now, first) {
        return CallNextHookEx(null_mut(), code, w, l);
    }
    if GetCursorPos(&mut old) == 0 || SetCursorPos(x, y) == 0 {
        set_status("Windows rejected pointer movement");
        return CallNextHookEx(null_mut(), code, w, l);
    }
    let focused = GetForegroundWindow() == root || SetForegroundWindow(root) != 0;
    let input = wheel_input(w as u32, event.mouseData);
    if SendInput(1, &input, size_of::<INPUT>() as i32) == 1 {
        STATE.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                s.count += 1;
                s.status = if focused {
                    "Focused gaze window"
                } else {
                    "Hover moved; Windows denied focus"
                };
            }
        });
        return 1; // Original suppressed only after successful replay.
    }
    SetCursorPos(old.x, old.y);
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.status = "Replay failed; normal scrolling";
        }
    });
    CallNextHookEx(null_mut(), code, w, l)
}
fn set_status(text: &'static str) {
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.status = text;
        }
    });
}
#[derive(Default)]
struct FocusLatch {
    target: usize,
    candidate: Option<(usize, u32)>,
}
impl FocusLatch {
    fn choose(&mut self, target: usize, now: u32, first: bool) -> bool {
        if first {
            self.target = target;
            self.candidate = None;
            return true;
        }
        if self.target == target {
            self.candidate = None;
            return false;
        }
        match self.candidate {
            Some((candidate, since)) if candidate == target && now.wrapping_sub(since) >= 120 => {
                self.target = target;
                self.candidate = None;
                true
            }
            Some((candidate, _)) if candidate == target => false,
            _ => {
                self.candidate = Some((target, now));
                false
            }
        }
    }
}
fn choose_target(target: HWND, now: u32, first: bool) -> bool {
    STATE.with(|s| {
        s.borrow_mut()
            .as_mut()
            .is_some_and(|s| s.focus.choose(target as usize, now, first))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gaze_switches_during_scroll_only_after_stable_new_window() {
        let mut f = FocusLatch::default();
        assert!(f.choose(1, 0, true));
        assert!(!f.choose(2, 10, false));
        assert!(!f.choose(1, 100, false));
        assert!(!f.choose(2, 110, false));
        assert!(!f.choose(2, 229, false));
        assert!(f.choose(2, 230, false));
        assert!(!f.choose(2, 300, false));
        assert!(f.choose(1, 350, true));
    }
    #[test]
    fn driver_generated_scroll_is_accepted_but_our_replay_cannot_recurse() {
        let mut event: MSLLHOOKSTRUCT = unsafe { zeroed() };
        event.mouseData = 120 << 16;
        for flags in [0, LLMHF_INJECTED, LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED] {
            event.flags = flags;
            event.dwExtraInfo = 42;
            assert!(accept_wheel(&event));
            event.dwExtraInfo = REPLAY_TAG;
            assert!(!accept_wheel(&event));
        }
        event.dwExtraInfo = 0;
        event.mouseData = 0;
        assert!(!accept_wheel(&event));
    }
    #[test]
    fn gesture_targets_once_until_half_second_pause_including_wrap() {
        let mut g = Gesture::default();
        assert!(g.first(u32::MAX - 100));
        assert!(!g.first(20));
        assert!(!g.first(519));
        assert!(g.first(1019));
    }
    #[test]
    fn replay_preserves_signed_high_resolution_delta_and_axis() {
        for delta in [-120_i16, -7, 1, 120] {
            for message in [WM_MOUSEWHEEL, WM_MOUSEHWHEEL] {
                let input = wheel_input(message, (delta as u16 as u32) << 16);
                let mi = unsafe { input.Anonymous.mi };
                assert_eq!(mi.mouseData as i32, delta as i32);
                assert_eq!(
                    mi.dwFlags,
                    if message == WM_MOUSEHWHEEL {
                        MOUSEEVENTF_HWHEEL
                    } else {
                        MOUSEEVENTF_WHEEL
                    }
                );
                assert_eq!(mi.dwExtraInfo, REPLAY_TAG);
            }
        }
    }
}
