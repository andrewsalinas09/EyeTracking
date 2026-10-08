//! A supported touchpad arms on contact and jumps on the first raw movement.
//! Taps never jump; further motion refines until all fingers lift to rearm.
//! Route real device handles to independent mouse bursts even with a touchpad
//! connected; only Windows' null-device touchpad motion uses contact state. This window
//! runs on its own thread so full-screen preview painting cannot stall input.
//! Precision touchpads can have a null hDevice; that is deliberately accepted.
use crate::{
    calibration::{Display, Model, Report},
    tobii::State,
};
use std::{
    collections::{HashMap, HashSet},
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemInformation::{GetTickCount, GetTickCount64},
    },
    UI::{
        Input::{KeyboardAndMouse::*, *},
        WindowsAndMessaging::*,
    },
};

const TOGGLE: u32 = WM_APP + 1;
const SET_REARM: u32 = WM_APP + 3; // +2 belongs to scroll::SCROLLED.
const SET_TRACKPAD_REARM: u32 = WM_APP + 4;
/// Read-only UI state. Input remains owned by its dedicated thread.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub mouse_rearm_ms: u32,
    pub trackpad_rearm_ms: u32,
    pub enabled: bool,
    pub dot: bool,
    pub scroll: bool,
    pub learning: bool,
    pub corner_trial: bool,
    pub corner_available: bool,
    pub learning_status: String,
    pub learning_storage: String,
    pub display_ok: bool,
    pub display: String,
    pub jumps: u64,
    pub accepted: u64,
    pub updates: u64,
    pub error: String,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum PointerSource {
    Touchpad,
    Mouse(usize),
}
impl PointerSource {
    fn from_raw(device: usize, contact_mode: bool) -> Self {
        if device == 0 && contact_mode {
            Self::Touchpad
        } else {
            Self::Mouse(device)
        }
    }
}
fn recent_click(last: Option<u32>, now: u32, interval: u32) -> bool {
    last.is_some_and(|t| now.wrapping_sub(t) < interval)
}
fn cooldown_ready(last: Option<u32>, now: u32, delay: u32) -> bool {
    last.is_none_or(|t| now.wrapping_sub(t) >= delay)
}
struct Burst {
    last: Option<u32>,
    delay_ms: u32,
}
impl Default for Burst {
    fn default() -> Self {
        Self {
            last: None,
            delay_ms: crate::preferences::DEFAULT_REARM_MS,
        }
    }
}
impl Burst {
    fn armed(&self, now: u32) -> bool {
        self.last
            .is_none_or(|t| now.wrapping_sub(t) >= self.delay_ms)
    }
    fn input(&mut self, now: u32, moved: bool, blocked: bool) -> bool {
        let jump = moved && !blocked && self.armed(now);
        if moved || blocked {
            self.last = Some(now);
        }
        jump
    }
}
struct Config {
    snapshot: Snapshot,
    display: Display,
    model: Option<Model>,
    calibrating: bool,
    learning: crate::learning::Learner,
}
impl Config {
    fn base_point(&self, point: [f64; 2]) -> [f64; 2] {
        let point = self.model.as_ref().map_or(point, |m| m.apply(point));
        let [left, top, right, bottom] = self.display.rect;
        [
            left as f64 + point[0] * (right - left) as f64,
            top as f64 + point[1] * (bottom - top) as f64,
        ]
    }
    // Shared by the desktop dot and the jump: both show/use the exact same map.
    fn screen_point(&self, point: [f64; 2]) -> [i32; 2] {
        let point = self.base_point(point);
        let offset = self.learning.offset_at(point, self.display.rect);
        let [left, top, right, bottom] = self.display.rect;
        [
            (point[0] + offset[0])
                .round()
                .clamp(left as f64, (right - 1) as f64) as i32,
            (point[1] + offset[1])
                .round()
                .clamp(top as f64, (bottom - 1) as f64) as i32,
        ]
    }
}
pub struct Controller {
    hwnd: usize,
    config: Arc<Mutex<Config>>,
    thread: Option<JoinHandle<()>>,
}
impl Controller {
    pub fn start(
        state: Arc<Mutex<State>>,
        display: Display,
        report: Option<&Report>,
        enabled: bool,
    ) -> Result<Self, String> {
        unsafe {
            let mut speed = 0u32;
            let mut accel = [0i32; 3];
            let speed_ok =
                SystemParametersInfoW(SPI_GETMOUSESPEED, 0, &mut speed as *mut _ as *mut _, 0) != 0;
            let accel_ok =
                SystemParametersInfoW(SPI_GETMOUSE, 0, accel.as_mut_ptr() as *mut _, 0) != 0;
            crate::capture::record(
                "input_environment",
                None,
                serde_json::json!({
                    "pointer_speed":speed_ok.then_some(speed),"pointer_acceleration":accel_ok.then_some(accel),
                    "double_click_ms":GetDoubleClickTime(),"virtual_screen":[GetSystemMetrics(SM_XVIRTUALSCREEN),GetSystemMetrics(SM_YVIRTUALSCREEN),GetSystemMetrics(SM_CXVIRTUALSCREEN),GetSystemMetrics(SM_CYVIRTUALSCREEN)],
                    "preferences":crate::preferences::Preferences::load(),"calibration_report":report,
                }),
            );
        }
        let config = Arc::new(Mutex::new(Config {
            snapshot: Snapshot {
                mouse_rearm_ms: crate::preferences::DEFAULT_REARM_MS,
                enabled,
                dot: true,
                scroll: true,
                learning: true,
                display_ok: true,
                ..Snapshot::default()
            },
            display: report.map_or(display, |r| r.display.clone()),
            model: report
                .filter(|r| r.metrics.recommend)
                .map(|r| r.model.clone()),
            calibrating: false,
            learning: crate::learning::Learner::default(),
        }));
        {
            let mut cfg = config.lock().unwrap();
            let display = cfg.display.clone();
            let model = cfg.model.clone();
            cfg.learning.start_recording(&display, model.as_ref());
        }
        let cfg = config.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || unsafe {
            let instance = GetModuleHandleW(null());
            let name = wide("EyeTrackingMouseStatus");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: name.as_ptr(),
                ..zeroed()
            };
            RegisterClassW(&wc);
            let rect = cfg.lock().unwrap().display.rect;
            let dot = match crate::gaze_dot::GazeDot::new() {
                Ok(dot) => dot,
                Err(error) => {
                    let _ = tx.send(Err(error));
                    return;
                }
            };
            let mut ctx = Box::new(Context {
                dot,
                dot_visible: true,
                state,
                config: cfg,
                enabled,
                scroll_enabled: true,
                mouse_rearm_ms: crate::preferences::DEFAULT_REARM_MS,
                bursts: HashMap::new(),
                trackpad_rearm_ms: 0,
                last_trackpad_landing: None,
                source: PointerSource::Touchpad,
                last_click: None,
                absolute: HashMap::new(),
                captured_devices: HashSet::new(),
                jumps: 0,
                misses: 0,
                motions: 0,
                last_error: String::new(),
                display_ok: true,
                touchpad: crate::touchpad::Touchpad::default(),
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                name.as_ptr(),
                wide("Gaze mouse status").as_ptr(),
                WS_POPUP | WS_DISABLED,
                rect[0] + 20,
                rect[1] + 20,
                820,
                292,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                let _ = tx.send(Err("Could not create mouse status window".into()));
                return;
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *ctx as *mut Context as isize);
            let device = RAWINPUTDEVICE {
                usUsagePage: 1,
                usUsage: 2,
                dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
                hwndTarget: hwnd,
            };
            // Shortcuts are optional conveniences; a collision must never disable input.
            for (id, key) in [(1, VK_F8), (2, VK_F9), (3, VK_F10), (4, VK_F7), (5, VK_F6)] {
                RegisterHotKey(hwnd, id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, key as u32);
            }
            if RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
                let _ = tx.send(Err(
                    "Could not register mouse input. Quit and reopen EyeTracking.".into(),
                ));
                DestroyWindow(hwnd);
                return;
            }
            SetLayeredWindowAttributes(hwnd, 0, 230, LWA_ALPHA);
            if !crate::touchpad::Touchpad::register(hwnd) {
                ctx.last_error = "Could not register Precision Touchpad input".into();
            }
            let _scroll = match crate::scroll::ScrollFocus::start(hwnd) {
                Ok(scroll) => Some(scroll),
                Err(error) => {
                    ctx.last_error = error;
                    None
                }
            };
            SetTimer(hwnd, 1, 50, None);
            SetTimer(hwnd, 2, 16, None);
            // Keep the input receiver alive while the status panel starts hidden.
            let _ = tx.send(Ok(hwnd as usize));
            let mut msg: MSG = zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
        match rx.recv().map_err(|e| e.to_string())? {
            Ok(hwnd) => Ok(Self {
                hwnd,
                config,
                thread: Some(thread),
            }),
            Err(e) => {
                let _ = thread.join();
                Err(e)
            }
        }
    }
    pub fn toggle(&self) {
        unsafe {
            PostMessageW(self.hwnd as HWND, TOGGLE, 0, 0);
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.config.lock().unwrap().snapshot.clone()
    }
    pub fn set_corner_trial(&self, enabled: bool) {
        let mut cfg = self.config.lock().unwrap();
        if cfg.learning.corner_enabled != enabled {
            cfg.learning.toggle_corner();
        }
        cfg.snapshot.corner_trial = cfg.learning.corner_enabled;
        cfg.snapshot.corner_available = cfg.learning.corner_available();
    }
    pub fn take_learning_feedback(&self) -> Option<crate::learning::Feedback> {
        self.config.lock().unwrap().learning.take_feedback()
    }
    pub fn set_rearm_delay(&self, ms: u32) {
        unsafe {
            PostMessageW(self.hwnd as HWND, SET_REARM, ms as usize, 0);
        }
    }
    pub fn set_trackpad_rearm_delay(&self, ms: u32) {
        unsafe {
            PostMessageW(self.hwnd as HWND, SET_TRACKPAD_REARM, ms as usize, 0);
        }
    }
    /// Same commands as the optional hotkeys, dispatched on the input thread.
    pub fn command(&self, id: usize) {
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_HOTKEY, id, 0);
        }
    }
    pub fn correction(&self, report: &Report, enabled: bool) {
        crate::capture::record(
            "calibration_change",
            None,
            serde_json::json!({"enabled":enabled,"report":report}),
        );
        let mut cfg = self.config.lock().unwrap();
        let cfg = &mut *cfg;
        cfg.model = enabled.then(|| report.model.clone());
        cfg.learning
            .change_context(&cfg.display, cfg.model.as_ref());
    }
    pub fn reset_learning(&self) -> Result<(), String> {
        let mut cfg = self.config.lock().unwrap();
        cfg.learning.reset_persistent()?;
        cfg.learning
            .record_context(&cfg.display, cfg.model.as_ref());
        Ok(())
    }
    pub fn open_learning_map(&self) -> Result<(), String> {
        self.config.lock().unwrap().learning.open_map()
    }
    pub fn offset(&self, display: &Display, base: [f32; 2]) -> [f32; 2] {
        let cfg = self.config.lock().unwrap();
        if cfg.display != *display || cfg.calibrating {
            return [0.0; 2];
        }
        let size = display.size();
        let point =
            std::array::from_fn(|axis| display.rect[axis] as f64 + base[axis] as f64 * size[axis]);
        let offset = cfg.learning.offset_at(point, display.rect);
        std::array::from_fn(|axis| (offset[axis] / size[axis]) as f32)
    }
    pub fn calibration(&self, active: bool, report: Option<&Report>) {
        crate::capture::record(
            "calibration_session",
            None,
            serde_json::json!({"active":active,"report":report}),
        );
        let mut cfg = self.config.lock().unwrap();
        let cfg = &mut *cfg;
        cfg.calibrating = active;
        cfg.learning.cancel("Calibration changed");
        if let Some(r) = report {
            cfg.display = r.display.clone();
            cfg.model = r.metrics.recommend.then(|| r.model.clone());
        }
        cfg.learning
            .change_context(&cfg.display, cfg.model.as_ref());
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
struct Context {
    trackpad_rearm_ms: u32,
    last_trackpad_landing: Option<u32>,
    mouse_rearm_ms: u32,
    scroll_enabled: bool,
    touchpad: crate::touchpad::Touchpad,
    dot: crate::gaze_dot::GazeDot,
    dot_visible: bool,
    state: Arc<Mutex<State>>,
    config: Arc<Mutex<Config>>,
    enabled: bool,
    bursts: HashMap<usize, Burst>,
    source: PointerSource,
    last_click: Option<u32>,
    absolute: HashMap<usize, (i32, i32)>,
    captured_devices: HashSet<usize>,
    jumps: u64,
    misses: u64,
    motions: u64,
    last_error: String,
    display_ok: bool,
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
unsafe fn buttons_down() -> bool {
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2]
        .iter()
        .any(|k| GetAsyncKeyState(*k as i32) < 0)
}
unsafe fn surface_at(point: POINT) -> usize {
    GetAncestor(WindowFromPoint(point), GA_ROOT) as usize
}
impl Context {
    fn trackpad_ready(&self, now: u32) -> bool {
        cooldown_ready(self.last_trackpad_landing, now, self.trackpad_rearm_ms)
    }
    fn armed(&self, now: u32) -> bool {
        match self.source {
            PointerSource::Touchpad => self.touchpad.armed() && self.trackpad_ready(now),
            PointerSource::Mouse(device) => self.bursts.get(&device).is_none_or(|b| b.armed(now)),
        }
    }
    fn gaze(&self) -> Option<[f64; 2]> {
        let state = self.state.lock().unwrap();
        if state.status != "Connected" {
            return None;
        }
        // Never reuse an older valid sample across a blink or tracking loss.
        state
            .samples
            .back()
            .filter(|s| s.valid && s.received.elapsed() <= Duration::from_millis(200))
            .map(|s| s.xy.map(|x| x as f64))
    }
    unsafe fn input(&mut self, l: LPARAM) {
        let modifiers = [VK_CONTROL, VK_MENU, VK_SHIFT]
            .iter()
            .any(|k| GetAsyncKeyState(*k as i32) < 0);
        let (allowed, point) = {
            let cfg = self.config.lock().unwrap();
            let allowed = self.enabled && !cfg.calibrating && self.display_ok;
            (
                allowed,
                if allowed {
                    self.gaze().map(|p| cfg.screen_point(p))
                } else {
                    None
                },
            )
        };
        if let Some(frames) = self.touchpad.input(
            l,
            !allowed
                || modifiers
                || buttons_down()
                || recent_click(self.last_click, GetTickCount(), GetDoubleClickTime()),
        ) {
            // Suppress only landing during the cooldown. Two-finger scrolling
            // remains independent, and a held contact can never jump later.
            if !self.trackpad_ready(GetTickCount()) {
                self.touchpad.motion(false, true);
            }
            crate::scroll::update(allowed && self.scroll_enabled, point, GetTickCount());
            for frame in frames {
                if frame.started || frame.scroll_start {
                    self.source = PointerSource::Touchpad;
                }
                if frame.multi {
                    self.config
                        .lock()
                        .unwrap()
                        .learning
                        .cancel("Skipped: two-finger gesture");
                }
                crate::scroll::touchpad(frame.multi, frame.scroll_start, frame.scrolling);
            }
            return;
        }
        let mut input: RAWINPUT = zeroed();
        let mut size = size_of::<RAWINPUT>() as u32;
        let n = GetRawInputData(
            l as HRAWINPUT,
            RID_INPUT,
            &mut input as *mut _ as *mut _,
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        );
        if n == u32::MAX
            || n < (size_of::<RAWINPUTHEADER>() + size_of::<RAWMOUSE>()) as u32
            || input.header.dwType != RIM_TYPEMOUSE
        {
            return;
        }
        let mouse = input.data.mouse;
        if self.captured_devices.insert(input.header.hDevice as usize) {
            crate::capture::input_device(input.header.hDevice);
        }
        let moved = if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
            let next = (mouse.lLastX, mouse.lLastY);
            self.absolute.insert(input.header.hDevice as usize, next) != Some(next)
        } else {
            mouse.lLastX != 0 || mouse.lLastY != 0
        };
        if moved {
            self.motions += 1;
        }
        let now = GetMessageTime() as u32;
        let flags = mouse.Anonymous.Anonymous.usButtonFlags;
        let source =
            PointerSource::from_raw(input.header.hDevice as usize, self.touchpad.contact_mode());
        if moved || flags != 0 {
            self.source = source;
        }
        if flags & 0x03ff != 0 {
            self.last_click = Some(now);
        }
        let mut cfg = self.config.lock().unwrap();
        let mut cursor: POINT = zeroed();
        let has_cursor = GetCursorPos(&mut cursor) != 0;
        let modifiers = [VK_CONTROL, VK_MENU, VK_SHIFT]
            .iter()
            .any(|k| GetAsyncKeyState(*k as i32) < 0);
        let surface = has_cursor.then(|| surface_at(cursor));
        let mut bounds: RECT = zeroed();
        let bounds_ok =
            flags != 0 && surface.is_some_and(|s| GetWindowRect(s as HWND, &mut bounds) != 0);
        crate::capture::record(
            if flags & 0x03ff != 0 {
                "click"
            } else if flags != 0 {
                "wheel_input"
            } else {
                "pointer"
            },
            cfg.learning.interaction(),
            serde_json::json!({
                "input_message_tick_ms":now,"processing_tick_ms":GetTickCount(),"queue_delay_ms":GetTickCount().wrapping_sub(now),
                "device_handle":input.header.hDevice as usize,"source":match source {PointerSource::Touchpad=>"touchpad",PointerSource::Mouse(_)=>"mouse"},
                "movement_flags":mouse.usFlags,"button_flags":flags,"button_data":mouse.Anonymous.Anonymous.usButtonData,
                "raw_buttons":mouse.ulRawButtons,"raw_delta_or_absolute":[mouse.lLastX,mouse.lLastY],"extra_information":mouse.ulExtraInformation,
                "cursor":has_cursor.then_some([cursor.x,cursor.y]),"surface":surface,"surface_rect":bounds_ok.then_some([bounds.left,bounds.top,bounds.right,bounds.bottom]),
            "foreground_surface":GetForegroundWindow() as usize,"target_dpi":if flags!=0{surface.map(|s|windows_sys::Win32::UI::HiDpi::GetDpiForWindow(s as HWND))}else{None},"modifier_keys":{"ctrl":GetAsyncKeyState(VK_CONTROL as i32)<0,"alt":GetAsyncKeyState(VK_MENU as i32)<0,"shift":GetAsyncKeyState(VK_SHIFT as i32)<0},
                "assist_enabled":self.enabled,"learning_enabled":cfg.learning.enabled,"calibrating":cfg.calibrating,"display_ok":self.display_ok,
                "rect":cfg.display.rect,"mouse_rearm_ms":self.mouse_rearm_ms,"trackpad_rearm_ms":self.trackpad_rearm_ms,
                "relation":"interaction is most recent jump, not a guaranteed label; includes clicks outside learning window",
            }),
        );
        if !self.enabled
            || cfg.calibrating
            || !self.display_ok
            || !has_cursor
            || modifiers
            || GetTickCount().wrapping_sub(now) > 100
        {
            cfg.learning
                .cancel("Skipped: paused, modifier or delayed input");
        } else {
            cfg.learning.event(
                now,
                [cursor.x as f64, cursor.y as f64],
                flags,
                surface_at(cursor),
            );
        }
        let blocked = buttons_down()
            || recent_click(self.last_click, now, GetDoubleClickTime())
            || flags != 0
            || !self.enabled
            || cfg.calibrating
            || modifiers
            || !self.display_ok
            || GetTickCount().wrapping_sub(now) > 100;
        let jump = match source {
            PointerSource::Touchpad => self
                .touchpad
                .motion(moved, blocked || !self.trackpad_ready(now)),
            PointerSource::Mouse(device) => self
                .bursts
                .entry(device)
                .or_insert(Burst {
                    last: None,
                    delay_ms: self.mouse_rearm_ms,
                })
                .input(now, moved, blocked),
        };
        if !jump {
            return;
        }
        if source == PointerSource::Touchpad {
            self.last_trackpad_landing = Some(now);
        }
        drop(cfg);
        self.jump(
            now,
            modifiers,
            if source == PointerSource::Touchpad {
                "slide"
            } else {
                "mouse"
            },
        );
    }
    unsafe fn jump(&mut self, now: u32, modifiers: bool, source: &str) {
        let mut cfg = self.config.lock().unwrap();
        if !self.enabled || cfg.calibrating || modifiers || buttons_down() {
            return;
        }
        let evidence = {
            let state = self.state.lock().unwrap();
            state
                .samples
                .back()
                .filter(|s| {
                    state.status == "Connected"
                        && s.valid
                        && s.received.elapsed() <= Duration::from_millis(200)
                })
                .map(|s| state.evidence(*s))
        };
        let point = evidence.map(|e| e.raw_gaze);
        cfg.learning.cancel("Replaced by next jump");
        if !self.display_ok || point.is_none() {
            self.misses += 1;
            self.last_error = if self.display_ok {
                "No fresh gaze at landing"
            } else {
                "Calibrated display changed"
            }
            .into();
            cfg.learning.record_jump(source, false);
            crate::capture::record(
                "jump",
                cfg.learning.interaction(),
                serde_json::json!({"source":source,"success":false,"reason":self.last_error,"input_message_tick_ms":now,"evidence":evidence,"rect":cfg.display.rect}),
            );
            return;
        }
        let [x, y] = cfg.screen_point(point.unwrap());
        let mut old: POINT = zeroed();
        let old_ok = GetCursorPos(&mut old) != 0;
        let base = cfg.base_point(point.unwrap());
        let learned = cfg.learning.offset_at(base, cfg.display.rect);
        let predicted = [base[0] + learned[0], base[1] + learned[1]];
        let warp_start = GetTickCount64();
        if SetCursorPos(x, y) != 0 {
            self.jumps += 1;
            cfg.learning.record_jump(source, true);
            self.last_error.clear();
            // Save pre-adaptation gaze at the jump. Later eye motion is not a label.
            let base = cfg.base_point(point.unwrap());
            let rect = cfg.display.rect;
            let mut actual: POINT = zeroed();
            let actual_ok = GetCursorPos(&mut actual) != 0;
            crate::capture::record(
                "jump",
                cfg.learning.interaction(),
                serde_json::json!({
                    "source":source,"success":true,"input_message_tick_ms":now,"warp_start_uptime_ms":warp_start,"warp_end_uptime_ms":GetTickCount64(),
                    "cursor_before":old_ok.then_some([old.x,old.y]),"base_unclamped":base,"learned_offset":learned,"prediction_unclamped":predicted,
                    "requested_landing":[x,y],"actual_landing":actual_ok.then_some([actual.x,actual.y]),"rect":rect,"evidence":evidence,
                    "clamped":predicted[0]<rect[0] as f64 || predicted[1]<rect[1] as f64 || predicted[0]>(rect[2]-1) as f64 || predicted[1]>(rect[3]-1) as f64,
                }),
            );
            if !modifiers && actual_ok && actual.x == x && actual.y == y {
                cfg.learning
                    .begin(now, base, [x as f64, y as f64], surface_at(actual), rect);
                if let Some(evidence) = evidence {
                    cfg.learning.attach_evidence(evidence);
                }
            }
        } else {
            let error = windows_sys::Win32::Foundation::GetLastError();
            self.misses += 1;
            self.last_error = "Windows rejected cursor movement".into();
            cfg.learning.record_jump(source, false);
            crate::capture::record(
                "jump",
                cfg.learning.interaction(),
                serde_json::json!({"source":source,"success":false,"reason":self.last_error,"windows_error":error,"input_message_tick_ms":now,"base_unclamped":base,"prediction_unclamped":predicted,"requested_landing":[x,y],"evidence":evidence}),
            );
        }
    }
    unsafe fn paint(&self, hwnd: HWND) {
        let mut ps: PAINTSTRUCT = zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut r: RECT = zeroed();
        GetClientRect(hwnd, &mut r);
        let brush = CreateSolidBrush(0x00251c16);
        FillRect(dc, &r, brush);
        DeleteObject(brush);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00e8e8e8);
        let font = CreateFontW(
            23,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            wide("Segoe UI").as_ptr(),
        );
        let old_font = SelectObject(dc, font);
        let cfg = self.config.lock().unwrap();
        let state = if !self.enabled {
            "PAUSED"
        } else if cfg.calibrating {
            "CALIBRATING"
        } else if !self.display_ok {
            "DISPLAY CHANGED"
        } else if self.gaze().is_none() {
            "NO GAZE"
        } else if buttons_down()
            || recent_click(self.last_click, GetTickCount(), GetDoubleClickTime())
        {
            "DRAG / CLICK"
        } else if self.armed(GetTickCount()) {
            if self.source == PointerSource::Touchpad {
                "ARMED - slide to land"
            } else {
                "ARMED - move to jump"
            }
        } else {
            "FINE CONTROL"
        };
        let learning = &cfg.learning;
        let (trained, total) = learning.coverage();
        let dot_state = if self.dot_visible { "ON" } else { "OFF" };
        let mode = if self.source == PointerSource::Touchpad {
            "Slide to land; lift to rearm"
        } else {
            "Motion: 300 ms rearm"
        };
        let text = wide(&format!("{state}    |    Ctrl+Alt+F8 pause/resume\n{mode}  |  {} jumps  |  {} missed  |  {}\n{} raw movements | Dot {dot_state}: Ctrl+Alt+F7 | {}\nLearn {}: {} clicks, {} updates | spatial {trained}/{total}\nCtrl+Alt+F9 freeze | Ctrl+Alt+F10 reset | {}\n{}\nTouchpad: {} reports, {} slides, {} scrolls\n{}",
            self.jumps, self.misses, if cfg.model.is_some() { "calibrated" } else { "raw gaze" },
            self.motions, self.last_error, if learning.enabled { "ON" } else { "FROZEN" },
            learning.accepted, learning.updates, learning.status, crate::scroll::status(), self.touchpad.reports, self.touchpad.slides, self.touchpad.gestures, learning.recording_status()));
        r.left += 12;
        r.top += 8;
        DrawTextW(dc, text.as_ptr(), -1, &mut r, DT_LEFT | DT_NOPREFIX);
        SelectObject(dc, old_font);
        DeleteObject(font);
        EndPaint(hwnd, &ps);
    }
}
unsafe extern "system" fn monitor_check(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let (display, matched) = &mut *(data as *mut (Display, bool));
    let mut info: MONITORINFOEXW = zeroed();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) != 0 {
        let r = info.monitorInfo.rcMonitor;
        let name = String::from_utf16_lossy(
            &info.szDevice[..info.szDevice.iter().position(|c| *c == 0).unwrap_or(32)],
        );
        *matched |= display.name == name && display.rect == [r.left, r.top, r.right, r.bottom];
    }
    1
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Context;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let ctx = &mut *ptr;
    if matches!(
        msg,
        WM_HOTKEY
            | SET_REARM
            | SET_TRACKPAD_REARM
            | TOGGLE
            | WM_INPUT_DEVICE_CHANGE
            | crate::scroll::SCROLLED
    ) {
        let mut cursor: POINT = zeroed();
        let cursor_ok = GetCursorPos(&mut cursor) != 0;
        crate::capture::record(
            "controller_event",
            None,
            serde_json::json!({"message":msg,"wparam":w,"lparam":l,"message_tick_ms":GetMessageTime() as u32,"cursor":cursor_ok.then_some([cursor.x,cursor.y])}),
        );
    }
    match msg {
        SET_TRACKPAD_REARM => {
            ctx.trackpad_rearm_ms = (w as u32).min(crate::preferences::MAX_REARM_MS);
            if !ctx.trackpad_ready(GetTickCount()) {
                ctx.touchpad.motion(false, true);
            }
            ctx.config.lock().unwrap().snapshot.trackpad_rearm_ms = ctx.trackpad_rearm_ms;
            0
        }
        SET_REARM => {
            ctx.mouse_rearm_ms = (w as u32).clamp(
                crate::preferences::MIN_REARM_MS,
                crate::preferences::MAX_REARM_MS,
            );
            for burst in ctx.bursts.values_mut() {
                burst.delay_ms = ctx.mouse_rearm_ms;
            }
            ctx.config.lock().unwrap().snapshot.mouse_rearm_ms = ctx.mouse_rearm_ms;
            0
        }
        WM_HOTKEY if w == 6 => {
            ctx.scroll_enabled = !ctx.scroll_enabled;
            if !ctx.scroll_enabled {
                crate::scroll::update(false, None, GetTickCount());
            }
            0
        }
        WM_INPUT_DEVICE_CHANGE if w == GIDC_REMOVAL as usize => {
            ctx.captured_devices.remove(&(l as usize));
            ctx.touchpad.removed(l as usize);
            ctx.bursts.remove(&(l as usize));
            ctx.absolute.remove(&(l as usize));
            0
        }
        crate::scroll::SCROLLED => {
            ctx.touchpad.motion(false, true);
            ctx.config
                .lock()
                .unwrap()
                .learning
                .cancel("Skipped: scrolling");
            if let PointerSource::Mouse(device) = ctx.source {
                ctx.bursts
                    .entry(device)
                    .or_insert(Burst {
                        last: None,
                        delay_ms: ctx.mouse_rearm_ms,
                    })
                    .last = Some(w as u32);
            }
            0
        }
        WM_INPUT => {
            ctx.input(l);
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_HOTKEY if w == 5 => {
            let show = IsWindowVisible(hwnd) == 0;
            ShowWindow(hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
            if show {
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_HOTKEY if w == 4 => {
            ctx.dot_visible = !ctx.dot_visible;
            if !ctx.dot_visible {
                ctx.dot.update(None, false);
            }
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_HOTKEY if w == 3 => {
            // Route the shortcut through the same confirmation as the button.
            let panel = FindWindowW(wide("EyeTrackingGazePreview").as_ptr(), null());
            if !panel.is_null() {
                PostMessageW(panel, WM_COMMAND, 110, 0);
            }
            0
        }
        WM_HOTKEY if w == 2 => {
            let mut cfg = ctx.config.lock().unwrap();
            cfg.learning.toggle();
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_HOTKEY | TOGGLE => {
            ctx.enabled = !ctx.enabled;
            ctx.touchpad.motion(false, true);
            ctx.config
                .lock()
                .unwrap()
                .learning
                .cancel("Mouse paused/resumed");
            ctx.bursts.clear();
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_TIMER if w == 2 => {
            let point = {
                let cfg = ctx.config.lock().unwrap();
                if cfg.calibrating || !ctx.display_ok {
                    None
                } else {
                    ctx.gaze().map(|p| cfg.screen_point(p))
                }
            };
            let now = GetTickCount();
            crate::scroll::update(ctx.enabled && ctx.scroll_enabled, point, now);
            let modifiers = [VK_CONTROL, VK_MENU, VK_SHIFT]
                .iter()
                .any(|k| GetAsyncKeyState(*k as i32) < 0);
            let ready = point.is_some()
                && ctx.enabled
                && !buttons_down()
                && !modifiers
                && ctx.armed(now)
                && !recent_click(ctx.last_click, now, GetDoubleClickTime());
            ctx.dot.update(point.filter(|_| ctx.dot_visible), ready);
            0
        }
        WM_TIMER => {
            let mut check = (ctx.config.lock().unwrap().display.clone(), false);
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(monitor_check),
                &mut check as *mut _ as LPARAM,
            );
            ctx.display_ok = check.1;
            {
                let mut cfg = ctx.config.lock().unwrap();
                if !ctx.display_ok {
                    cfg.learning.cancel("Display changed");
                }
                cfg.learning.expire(GetTickCount());
                cfg.snapshot = Snapshot {
                    trackpad_rearm_ms: ctx.trackpad_rearm_ms,
                    mouse_rearm_ms: ctx.mouse_rearm_ms,
                    enabled: ctx.enabled,
                    dot: ctx.dot_visible,
                    scroll: ctx.scroll_enabled,
                    learning: cfg.learning.enabled,
                    corner_trial: cfg.learning.corner_enabled,
                    corner_available: cfg.learning.corner_available(),
                    learning_status: cfg.learning.status.into(),
                    learning_storage: cfg.learning.storage_status(),
                    display_ok: ctx.display_ok,
                    display: cfg.display.name.clone(),
                    jumps: ctx.jumps,
                    accepted: cfg.learning.accepted,
                    updates: cfg.learning.updates,
                    error: ctx.last_error.clone(),
                };
            }
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_PAINT => {
            ctx.paint(hwnd);
            0
        }
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_ERASEBKGND => 1,
        WM_DESTROY => {
            ctx.config
                .lock()
                .unwrap()
                .learning
                .cancel("Skipped: app closed");
            UnregisterHotKey(hwnd, 1);
            UnregisterHotKey(hwnd, 2);
            UnregisterHotKey(hwnd, 3);
            UnregisterHotKey(hwnd, 4);
            UnregisterHotKey(hwnd, 5);
            KillTimer(hwnd, 1);
            KillTimer(hwnd, 2);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trackpad_cooldown_defaults_to_immediate_and_wraps_safely() {
        assert!(cooldown_ready(None, 0, 2000));
        assert!(cooldown_ready(Some(100), 100, 0));
        assert!(!cooldown_ready(Some(100), 399, 300));
        assert!(cooldown_ready(Some(100), 400, 300));
        assert!(cooldown_ready(Some(u32::MAX - 49), 0, 50));
    }
    #[test]
    fn configurable_rearm_changes_the_threshold_without_resetting_motion_history() {
        let mut b = Burst {
            last: None,
            delay_ms: 100,
        };
        assert!(b.input(0, true, false));
        assert!(!b.input(99, true, false));
        assert!(b.input(199, true, false));
        b.delay_ms = 750;
        assert!(!b.armed(948));
        assert!(b.armed(949));
        b.last = Some(u32::MAX - 49);
        b.delay_ms = 50;
        assert!(b.armed(0));
    }
    #[test]
    fn physical_mouse_keeps_its_own_burst_when_touchpad_is_present() {
        assert_eq!(PointerSource::from_raw(0, true), PointerSource::Touchpad);
        assert_eq!(PointerSource::from_raw(0, false), PointerSource::Mouse(0));
        assert_eq!(
            PointerSource::from_raw(123, true),
            PointerSource::Mouse(123)
        );
        let mut bursts = HashMap::<usize, Burst>::new();
        for (device, now, expected) in [
            (123, 0, true),
            (123, 1, false),
            (456, 2, true),
            (123, 301, true),
        ] {
            let PointerSource::Mouse(device) = PointerSource::from_raw(device, true) else {
                panic!("Mouse must not consume touchpad contact state")
            };
            assert_eq!(
                bursts.entry(device).or_default().input(now, true, false),
                expected
            );
        }
    }
    #[test]
    fn double_click_protection_uses_system_interval_and_handles_clock_wrap() {
        assert!(!recent_click(None, 0, 500));
        assert!(recent_click(Some(100), 599, 500));
        assert!(!recent_click(Some(100), 600, 500));
        assert!(recent_click(Some(u32::MAX - 100), 20, 500));
    }
    #[test]
    fn every_burst_jumps_once_including_tiny_moves_and_long_fine_control() {
        let mut b = Burst::default();
        assert!(b.input(0, true, false));
        for t in 1..1000 {
            assert!(!b.input(t, true, false));
        }
        assert!(!b.input(1298, true, false));
        assert!(b.input(1598, true, false));
        for n in 1..1000 {
            assert!(b.input(1598 + n * 300, true, false));
        }
    }
    #[test]
    fn clicks_scrolls_drags_and_empty_packets_cannot_jump() {
        let mut b = Burst::default();
        assert!(!b.input(0, false, false));
        assert!(b.armed(0));
        assert!(!b.input(100, true, true));
        assert!(!b.input(400, false, true));
        assert!(!b.input(500, true, false));
        assert!(b.input(800, true, false));
    }
    #[test]
    fn missed_gaze_consumes_attempt_and_clock_wrap_is_safe() {
        let mut b = Burst::default();
        assert!(b.input(u32::MAX - 100, true, false)); // This attempt can lack gaze.
        assert!(!b.input(20, true, false)); // No delayed jump when gaze recovers.
        assert!(b.input(320, true, false));
    }
}
