//! Notification icon lifecycle, including Explorer restarts and keyboard activation.
use super::*;
use windows_sys::Win32::UI::Shell::*;

pub const CALLBACK: u32 = WM_APP + 20;
pub const OPEN: u32 = WM_APP + 21;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
pub struct Tray {
    data: NOTIFYICONDATAW,
    pub available: bool,
    pub restart_message: u32,
    tooltip: String,
}
impl Tray {
    pub unsafe fn new(hwnd: HWND) -> Self {
        let mut data: NOTIFYICONDATAW = zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = icon();
        let mut tray = Self {
            data,
            available: false,
            restart_message: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            tooltip: String::new(),
        };
        tray.set_tip("EyeTracking — opening control panel");
        tray.add();
        SendMessageW(
            hwnd,
            WM_SETICON,
            ICON_SMALL as usize,
            tray.data.hIcon as isize,
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            ICON_BIG as usize,
            tray.data.hIcon as isize,
        );
        tray
    }
    pub unsafe fn add(&mut self) {
        self.available = Shell_NotifyIconW(NIM_ADD, &self.data) != 0;
        if self.available {
            self.data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &self.data);
        }
    }
    pub unsafe fn set_tip(&mut self, text: &str) {
        if self.tooltip == text {
            return;
        }
        self.tooltip = text.into();
        self.data.szTip.fill(0);
        for (slot, ch) in self
            .data
            .szTip
            .iter_mut()
            .take(127)
            .zip(text.encode_utf16())
        {
            *slot = ch;
        }
        if self.available {
            Shell_NotifyIconW(NIM_MODIFY, &self.data);
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.data);
            DestroyIcon(self.data.hIcon);
        }
    }
}
unsafe fn icon() -> HICON {
    let mut pm = Pixmap::new(32, 32).unwrap();
    circle(&mut pm, 16.0, 16.0, 15.0, [105, 230, 194, 255], None);
    let mut p = PathBuilder::new();
    p.move_to(5.0, 16.0);
    p.cubic_to(11.0, 6.0, 21.0, 6.0, 27.0, 16.0);
    p.cubic_to(21.0, 26.0, 11.0, 26.0, 5.0, 16.0);
    p.close();
    let mut paint = Paint::default();
    paint.set_color_rgba8(16, 28, 38, 255);
    pm.fill_path(
        &p.finish().unwrap(),
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    circle(&mut pm, 16.0, 16.0, 4.0, [105, 230, 194, 255], None);
    for pixel in pm.data_mut().chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let handle = CreateIcon(
        GetModuleHandleW(null()),
        32,
        32,
        1,
        32,
        [0u8; 128].as_ptr(),
        pm.data().as_ptr(),
    );
    if handle.is_null() {
        CopyIcon(LoadIconW(null_mut(), IDI_APPLICATION))
    } else {
        handle
    }
}

pub unsafe fn callback(hwnd: HWND, w: WPARAM, l: LPARAM) {
    match (l as u32) & 0xffff {
        NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => {
            PostMessageW(hwnd, OPEN, 0, 0);
        }
        WM_CONTEXTMENU | WM_RBUTTONUP => {
            let snapshot = APP.with(|a| {
                a.borrow()
                    .as_ref()
                    .and_then(|a| a.mouse.as_ref())
                    .map(|m| m.snapshot())
            });
            let menu = CreatePopupMenu();
            AppendMenuW(
                menu,
                MF_STRING,
                dashboard::HOME as usize,
                wide("Open EyeTracking").as_ptr(),
            );
            if let Some(s) = snapshot {
                AppendMenuW(
                    menu,
                    MF_STRING,
                    dashboard::POWER as usize,
                    wide(if s.enabled {
                        "Pause gaze control"
                    } else {
                        "Resume gaze control"
                    })
                    .as_ptr(),
                );
                AppendMenuW(
                    menu,
                    MF_STRING | if s.dot { MF_CHECKED } else { 0 },
                    dashboard::DOT as usize,
                    wide("Show gaze dot").as_ptr(),
                );
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(
                menu,
                MF_STRING,
                dashboard::QUIT as usize,
                wide("Quit EyeTracking").as_ptr(),
            );
            SetForegroundWindow(hwnd);
            let mut point = POINT {
                x: w as u16 as i16 as i32,
                y: (w >> 16) as u16 as i16 as i32,
            };
            if point.x == -1 && point.y == -1 {
                GetCursorPos(&mut point);
            }
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                null(),
            );
            DestroyMenu(menu);
            PostMessageW(hwnd, WM_NULL, 0, 0);
            if command != 0 {
                PostMessageW(hwnd, WM_COMMAND, command as usize, 0);
            }
        }
        _ => {}
    }
}
