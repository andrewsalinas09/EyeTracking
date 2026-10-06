//! Tiny desktop overlay. A layered, nonactivating window keeps all mouse input
//! going to the app underneath; the white rim makes black visible on dark pages.
use std::{
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::*, System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};

const SIZE: i32 = 9;
const TRANSPARENT_COLOR: u32 = 0x00ff00ff;
pub struct GazeDot {
    hwnd: HWND,
    position: Option<[i32; 2]>,
}
impl GazeDot {
    pub unsafe fn new() -> Result<Self, String> {
        let class: Vec<u16> = "EyeTrackingGazeDot\0".encode_utf16().collect();
        let title: Vec<u16> = "Gaze position\0".encode_utf16().collect();
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_DISABLED,
            0,
            0,
            SIZE,
            SIZE,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err("Could not create gaze dot overlay".into());
        }
        if SetLayeredWindowAttributes(hwnd, TRANSPARENT_COLOR, 255, LWA_COLORKEY) == 0 {
            DestroyWindow(hwnd);
            return Err("Could not make gaze dot transparent".into());
        }
        Ok(Self {
            hwnd,
            position: None,
        })
    }
    pub unsafe fn update(&mut self, position: Option<[i32; 2]>) {
        if position == self.position {
            return;
        }
        self.position = position;
        if let Some([x, y]) = position {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x - SIZE / 2,
                y - SIZE / 2,
                SIZE,
                SIZE,
                SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOSIZE,
            );
        } else {
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}
impl Drop for GazeDot {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let r = RECT {
                left: 0,
                top: 0,
                right: SIZE,
                bottom: SIZE,
            };
            let background = CreateSolidBrush(TRANSPARENT_COLOR);
            FillRect(dc, &r, background);
            DeleteObject(background);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            let old_brush = SelectObject(dc, GetStockObject(WHITE_BRUSH));
            Ellipse(dc, 0, 0, SIZE, SIZE);
            SelectObject(dc, GetStockObject(BLACK_BRUSH));
            Ellipse(dc, 1, 1, SIZE - 1, SIZE - 1);
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
