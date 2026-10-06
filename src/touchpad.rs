//! Precision Touchpad scrolling may bypass mouse wheel hooks entirely. Observe
//! digitizer reports passively, using the HID descriptor parser from TrackpadGlass.
//! Contact arms the next raw pointer movement; touch alone must never warp a tap
//! or double tap away from its target. Lift, not an idle timeout, rearms sliding.
use std::{
    collections::HashMap,
    mem::{offset_of, size_of, zeroed},
    ptr::null_mut,
};
use windows_sys::Win32::{Devices::HumanInterfaceDevice::*, Foundation::*, UI::Input::*};
#[derive(Clone, Copy)]
struct Contact {
    id: u32,
    xy: [f64; 2],
}
#[derive(Default)]
struct Landing {
    occupied: bool,
    pending: bool,
}
impl Landing {
    fn frame(&mut self, contacts: usize, blocked: bool) {
        if contacts == 0 {
            *self = Self::default();
            return;
        }
        if !self.occupied {
            self.pending = contacts == 1 && !blocked;
        }
        if blocked || contacts != 1 {
            self.pending = false;
        }
        self.occupied = contacts != 0;
    }
    fn motion(&mut self, moved: bool, blocked: bool) -> bool {
        let jump = moved && !blocked && self.pending;
        if moved || blocked {
            self.pending = false;
        }
        jump
    }
}
#[derive(Default, serde::Serialize)]
pub struct Frame {
    pub contacts: Option<usize>,
    pub started: bool,
    pub armed: bool,
    pub multi: bool,
    pub scroll_start: bool,
    pub scrolling: bool,
}
#[derive(Default)]
struct Gesture {
    start: Option<[Contact; 2]>,
    used: bool,
    scrolling: bool,
}
impl Gesture {
    fn frame(&mut self, contacts: &[Contact], blocked: bool) -> bool {
        if contacts.is_empty() {
            *self = Self::default();
            return false;
        }
        if blocked || contacts.len() > 2 {
            self.used = true;
            self.scrolling = false;
        }
        if contacts.len() != 2 {
            self.scrolling = false;
        }
        if self.used || contacts.len() != 2 {
            return false;
        }
        let mut pair = [contacts[0], contacts[1]];
        pair.sort_by_key(|p| p.id);
        let Some(start) = self.start else {
            self.start = Some(pair);
            return false;
        };
        if pair[0].id != start[0].id || pair[1].id != start[1].id {
            self.used = true;
            return false;
        }
        let a = [
            pair[0].xy[0] - start[0].xy[0],
            pair[0].xy[1] - start[0].xy[1],
        ];
        let b = [
            pair[1].xy[0] - start[1].xy[0],
            pair[1].xy[1] - start[1].xy[1],
        ];
        let na = a[0].hypot(a[1]);
        let nb = b[0].hypot(b[1]);
        if na < 0.0015 || nb < 0.0015 {
            return false;
        }
        let parallel = a[0] * b[0] + a[1] * b[1] > 0.8 * na * nb && na.min(nb) / na.max(nb) > 0.4;
        self.used = true; // Pinches and rotations also stay ignored until lift-off.
        self.scrolling = parallel;
        parallel
    }
}
struct Device {
    pp: Vec<u64>,
    slots: Vec<u16>,
    ranges: [[f64; 2]; 2],
    gesture: Gesture,
    landing: Landing,
    seen: bool,
}
unsafe fn value(
    pp: PHIDP_PREPARSED_DATA,
    page: u16,
    collection: u16,
    usage: u16,
    report: &mut [u8],
) -> Option<u32> {
    let mut v = 0;
    (HidP_GetUsageValue(
        HidP_Input,
        page,
        collection,
        usage,
        &mut v,
        pp,
        report.as_mut_ptr(),
        report.len() as u32,
    ) == HIDP_STATUS_SUCCESS)
        .then_some(v)
}
unsafe fn usages(
    pp: PHIDP_PREPARSED_DATA,
    page: u16,
    collection: u16,
    report: &mut [u8],
) -> Option<Vec<u16>> {
    let mut list = vec![0; 64];
    let mut n = list.len() as u32;
    if HidP_GetUsages(
        HidP_Input,
        page,
        collection,
        list.as_mut_ptr(),
        &mut n,
        pp,
        report.as_mut_ptr(),
        report.len() as u32,
    ) != HIDP_STATUS_SUCCESS
    {
        return None;
    }
    list.truncate(n as usize);
    Some(list)
}
impl Device {
    unsafe fn new(handle: HANDLE) -> Option<Self> {
        let mut size = 0;
        if GetRawInputDeviceInfoW(handle, RIDI_PREPARSEDDATA, null_mut(), &mut size) == u32::MAX
            || size == 0
            || size > 65536
        {
            return None;
        }
        let mut pp = vec![0u64; (size as usize).div_ceil(8)];
        if GetRawInputDeviceInfoW(
            handle,
            RIDI_PREPARSEDDATA,
            pp.as_mut_ptr() as *mut _,
            &mut size,
        ) == u32::MAX
        {
            return None;
        }
        let ptr = pp.as_ptr() as PHIDP_PREPARSED_DATA;
        let mut caps: HIDP_CAPS = zeroed();
        if HidP_GetCaps(ptr, &mut caps) != HIDP_STATUS_SUCCESS
            || caps.UsagePage != 0x0d
            || caps.Usage != 5
        {
            return None;
        }
        let mut n = caps.NumberInputValueCaps;
        let mut values = vec![zeroed::<HIDP_VALUE_CAPS>(); n as usize];
        if HidP_GetValueCaps(HidP_Input, values.as_mut_ptr(), &mut n, ptr) != HIDP_STATUS_SUCCESS {
            return None;
        }
        let mut slots = Vec::new();
        let mut ranges = [[0.0; 2]; 2];
        for c in values.iter().take(n as usize) {
            let u = if c.IsRange != 0 {
                c.Anonymous.Range.UsageMin
            } else {
                c.Anonymous.NotRange.Usage
            };
            if c.UsagePage == 0x0d && u == 0x51 {
                slots.push(c.LinkCollection);
            }
            if c.UsagePage == 1 && matches!(u, 0x30 | 0x31) && c.LinkCollection != 0 {
                ranges[(u - 0x30) as usize] = [c.LogicalMin as f64, c.LogicalMax as f64];
            }
        }
        slots.sort_unstable();
        slots.dedup();
        if slots.is_empty() || ranges.iter().any(|r| r[1] <= r[0]) {
            return None;
        }
        Some(Self {
            pp,
            slots,
            ranges,
            gesture: Gesture::default(),
            landing: Landing::default(),
            seen: false,
        })
    }
    fn uncertain(&mut self) -> Frame {
        // A malformed/palm report is not evidence of lift-off. Require an
        // explicit empty contact frame before permitting another landing.
        self.landing.occupied = true;
        self.landing.pending = false;
        self.gesture.used = true;
        self.gesture.scrolling = false;
        Frame {
            multi: true,
            ..Frame::default()
        }
    }
    unsafe fn frame(&mut self, report: &mut [u8], blocked: bool) -> Option<Frame> {
        let pp = self.pp.as_ptr() as PHIDP_PREPARSED_DATA;
        let count = value(pp, 0x0d, 0, 0x54, report)? as usize;
        self.seen = true;
        // This parser handles parallel reports, as exposed by this Magic Trackpad.
        if count > self.slots.len() {
            self.gesture.used = true;
            return None;
        }
        let button = usages(pp, 9, 0, report)?.contains(&1);
        let mut contacts = Vec::new();
        for &slot in self.slots.iter().take(count) {
            let flags = usages(pp, 0x0d, slot, report)?;
            if !flags.contains(&0x42) {
                continue;
            }
            if !flags.contains(&0x47) {
                return Some(self.uncertain());
            }
            let id = value(pp, 0x0d, slot, 0x51, report)?;
            let x = value(pp, 1, slot, 0x30, report)? as f64;
            let y = value(pp, 1, slot, 0x31, report)? as f64;
            let xy = std::array::from_fn(|a| {
                ([x, y][a] - self.ranges[a][0]) / (self.ranges[a][1] - self.ranges[a][0])
            });
            if xy.iter().any(|v| !(0.0..=1.0).contains(v)) {
                self.gesture.used = true;
                return None;
            }
            contacts.push(Contact { id, xy });
        }
        let multi = contacts.len() >= 2;
        let started = !contacts.is_empty() && !self.landing.occupied;
        let trigger = self.gesture.frame(&contacts, blocked || button);
        self.landing.frame(contacts.len(), blocked || button);
        Some(Frame {
            contacts: Some(contacts.len()),
            started,
            armed: self.landing.pending,
            multi,
            scroll_start: trigger,
            scrolling: self.gesture.scrolling,
        })
    }
}
#[derive(Default)]
pub struct Touchpad {
    devices: HashMap<usize, Option<Device>>,
    pub reports: u64,
    pub gestures: u64,
    pub slides: u64,
}
impl Touchpad {
    pub fn contact_mode(&self) -> bool {
        self.devices.values().flatten().any(|d| d.seen)
    }
    pub fn armed(&self) -> bool {
        let mut occupied = self
            .devices
            .values()
            .flatten()
            .filter(|d| d.landing.occupied);
        match occupied.next() {
            None => true,
            Some(d) => d.landing.pending && occupied.next().is_none(),
        }
    }
    pub fn motion(&mut self, moved: bool, blocked: bool) -> bool {
        let blocked = blocked || !self.armed();
        let mut jump = false;
        for device in self.devices.values_mut().flatten() {
            jump |= device.landing.motion(moved, blocked);
        }
        self.slides += u64::from(jump);
        jump
    }
    pub unsafe fn register(hwnd: HWND) -> bool {
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x0d,
            usUsage: 5,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        };
        RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) != 0
    }
    pub fn removed(&mut self, device: usize) {
        self.devices.remove(&device);
    }
    pub unsafe fn input(&mut self, l: LPARAM, blocked: bool) -> Option<Vec<Frame>> {
        let mut header: RAWINPUTHEADER = zeroed();
        let mut hs = size_of::<RAWINPUTHEADER>() as u32;
        if GetRawInputData(
            l as HRAWINPUT,
            RID_HEADER,
            &mut header as *mut _ as *mut _,
            &mut hs,
            size_of::<RAWINPUTHEADER>() as u32,
        ) == u32::MAX
            || header.dwType != RIM_TYPEHID
        {
            return None;
        }
        let mut size = 0;
        GetRawInputData(l as HRAWINPUT, RID_INPUT, null_mut(), &mut size, hs);
        if size < hs + 8 || size > 65536 {
            return Some(Vec::new());
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        if GetRawInputData(
            l as HRAWINPUT,
            RID_INPUT,
            buf.as_mut_ptr() as *mut _,
            &mut size,
            hs,
        ) != size
        {
            return Some(Vec::new());
        }
        let bytes = std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, size as usize);
        let data = offset_of!(RAWINPUT, data);
        let report_size = u32::from_le_bytes(bytes[data..data + 4].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(bytes[data + 4..data + 8].try_into().unwrap()) as usize;
        let offset = data + offset_of!(RAWHID, bRawData);
        if report_size == 0 || offset > bytes.len() || count > (bytes.len() - offset) / report_size
        {
            return Some(Vec::new());
        }
        let Some(dev) = self
            .devices
            .entry(header.hDevice as usize)
            .or_insert_with(|| Device::new(header.hDevice))
        else {
            return Some(Vec::new());
        };
        let mut frames = Vec::new();
        for report in bytes[offset..offset + report_size * count].chunks_exact_mut(report_size) {
            let frame = dev
                .frame(report, blocked)
                .unwrap_or_else(|| dev.uncertain());
            self.reports += 1;
            self.gestures += u64::from(frame.scroll_start);
            frames.push(frame);
        }
        Some(frames)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tap_and_double_tap_do_not_jump_but_first_slide_does() {
        let mut l = Landing::default();
        for _ in 0..2 {
            l.frame(1, false);
            assert!(!l.motion(false, false));
            l.frame(0, false);
            assert!(!l.motion(false, true)); // Windows' synthesized tap click.
        }
        l.frame(1, false);
        assert!(l.motion(true, false));
        for _ in 0..10000 {
            l.frame(1, false);
            assert!(!l.motion(true, false));
        }
        l.frame(0, false);
        assert!(!l.motion(true, false)); // Late emulated packet after lift.
        l.frame(1, false);
        assert!(l.motion(true, false));
    }
    #[test]
    fn blocked_or_multi_contact_requires_full_lift_even_after_unblocking() {
        for contacts in [1, 2, 3] {
            let mut l = Landing::default();
            l.frame(contacts, true);
            l.frame(1, false);
            assert!(!l.motion(true, false));
            l.frame(0, false);
            l.frame(1, false);
            assert!(l.motion(true, false));
        }
        let mut l = Landing::default();
        l.frame(2, false);
        l.frame(1, false);
        assert!(!l.motion(true, false));
        l.frame(0, false);
        l.frame(1, false);
        assert!(!l.motion(true, true)); // Click/drag consumes the pending slide.
        assert!(!l.motion(true, false));
        l.frame(0, false);
        l.frame(1, false);
        assert!(l.motion(true, false));
    }
    fn pair(y: f64) -> [Contact; 2] {
        [
            Contact {
                id: 1,
                xy: [0.3, y],
            },
            Contact {
                id: 2,
                xy: [0.6, y],
            },
        ]
    }
    #[test]
    fn two_finger_scroll_targets_once_and_rearms_only_after_lift() {
        let mut g = Gesture::default();
        assert!(!g.frame(&pair(0.4), false));
        assert!(!g.frame(&pair(0.401), false));
        assert!(g.frame(&pair(0.403), false));
        assert!(!g.frame(&pair(0.41), false));
        assert!(!g.frame(&pair(0.41)[..1], false));
        assert!(!g.frame(&pair(0.42), false));
        g.frame(&[], false);
        g.frame(&pair(0.5), false);
        assert!(g.frame(&pair(0.49), false));
    }
    #[test]
    fn pinch_click_three_fingers_and_replaced_contacts_do_not_focus() {
        for kind in 0..4 {
            let mut g = Gesture::default();
            g.frame(&pair(0.4), false);
            let mut p = pair(0.41).to_vec();
            match kind {
                0 => {
                    p[0].xy[1] = 0.39;
                }
                1 => {}
                2 => p.push(Contact {
                    id: 3,
                    xy: [0.4, 0.4],
                }),
                _ => p[1].id = 8,
            }
            assert!(!g.frame(&p, kind == 1));
            assert!(!g.frame(&pair(0.45), false));
        }
    }
}
