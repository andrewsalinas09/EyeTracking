//! Optional, click-through learning evidence, painted on the UI thread.
//! This overlay never injects input or changes the learner. Its moving marker
//! travels only the measured map delta, not the whole distance to the click.
use crate::learning::Feedback;
use std::{
    mem::zeroed,
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::*, System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};

const KEY: u32 = 0x00ff00ff;
const ORANGE: u32 = 0x0063b5f8;
const MINT: u32 = 0x00e5e860;
const WHITE: u32 = 0x00faf3ea;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn ease(t: f64) -> f64 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
fn animated_point(event: &Feedback, seconds: f64) -> [f64; 2] {
    let amount = ease((seconds - 0.4) / 0.65);
    std::array::from_fn(|i| event.before[i] + (event.after[i] - event.before[i]) * amount)
}
fn caption(event: &Feedback) -> (String, String) {
    if event.demo {
        return (
            "Preview · simulated learning update".into(),
            "Animation only — no cursor movement or learning".into(),
        );
    }
    if event.updated {
        let delta = (event.after[0] - event.before[0]).hypot(event.after[1] - event.before[1]);
        (
            format!("Learned · adjustment {delta:.1} px"),
            "Orange: landed   White: clicked   Mint: new prediction".into(),
        )
    } else if event.accepted {
        (
            "Click saved · no map change yet".into(),
            event.reason.into(),
        )
    } else {
        (
            "Not learned · no map change".into(),
            event
                .reason
                .strip_prefix("Skipped: ")
                .unwrap_or(event.reason)
                .into(),
        )
    }
}
// Size is limited to a small neighborhood even for rejected long movements.
// Fit the label at screen edges and support monitors with negative origins.
fn bounds(event: &Feedback, scale: f64) -> [i32; 4] {
    let width = (390. * scale).round() as i32;
    let height = (230. * scale).round() as i32;
    let width = width.min(event.rect[2] - event.rect[0]);
    let height = height.min(event.rect[3] - event.rect[1]);
    let anchor = event.selection.unwrap_or(event.landed);
    let x = (anchor[0].round() as i32 - width / 2).clamp(event.rect[0], event.rect[2] - width);
    let y = (anchor[1].round() as i32 - height / 2).clamp(event.rect[1], event.rect[3] - height);
    [x, y, width, height]
}
struct Frame {
    event: Feedback,
    start: Instant,
    bounds: [i32; 4],
    scale: f64,
}
struct State {
    frame: Option<Frame>,
}
pub struct Overlay {
    hwnd: HWND,
    state: Box<State>,
}
impl Overlay {
    pub unsafe fn new() -> Result<Self, String> {
        let class = wide("EyeTrackingLearningFeedback");
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
            wide("Learning feedback").as_ptr(),
            WS_POPUP | WS_DISABLED,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err("Could not create learning feedback overlay".into());
        }
        if SetLayeredWindowAttributes(hwnd, KEY, 255, LWA_COLORKEY | LWA_ALPHA) == 0 {
            DestroyWindow(hwnd);
            return Err("Could not make learning feedback transparent".into());
        }
        let mut state = Box::new(State { frame: None });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *state as *mut State as isize);
        Ok(Self { hwnd, state })
    }
    pub unsafe fn show(&mut self, event: Feedback, scale: f64) {
        let b = bounds(&event, scale);
        self.state.frame = Some(Frame {
            event,
            start: Instant::now(),
            bounds: b,
            scale,
        });
        SetLayeredWindowAttributes(self.hwnd, KEY, 255, LWA_COLORKEY | LWA_ALPHA);
        SetWindowPos(
            self.hwnd,
            HWND_TOPMOST,
            b[0],
            b[1],
            b[2],
            b[3],
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        InvalidateRect(self.hwnd, null(), 0);
    }
    pub unsafe fn hide(&mut self) {
        if self.state.frame.take().is_some() {
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
    pub unsafe fn tick(&mut self) {
        let Some(frame) = &self.state.frame else {
            return;
        };
        let seconds = frame.start.elapsed().as_secs_f64();
        let duration = if frame.event.accepted || frame.event.demo {
            2.4
        } else {
            1.5
        };
        if seconds >= duration {
            self.hide();
            return;
        }
        let alpha = (255. * (1. - ease((seconds - (duration - 0.4)) / 0.4))).round() as u8;
        SetLayeredWindowAttributes(self.hwnd, KEY, alpha, LWA_COLORKEY | LWA_ALPHA);
        InvalidateRect(self.hwnd, null(), 0);
    }
}
impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
        }
    }
}
unsafe fn ring(dc: HDC, p: [i32; 2], radius: i32, color: u32, fill: bool, thickness: i32) {
    let pen = CreatePen(PS_SOLID, thickness, color);
    let brush = if fill {
        CreateSolidBrush(color)
    } else {
        GetStockObject(NULL_BRUSH)
    };
    let old_pen = SelectObject(dc, pen);
    let old_brush = SelectObject(dc, brush);
    Ellipse(
        dc,
        p[0] - radius,
        p[1] - radius,
        p[0] + radius + 1,
        p[1] + radius + 1,
    );
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    DeleteObject(pen);
    if fill {
        DeleteObject(brush);
    }
}
unsafe fn text(dc: HDC, x: i32, y: i32, size: i32, color: u32, value: &str) {
    let font = CreateFontW(
        -size,
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        ANTIALIASED_QUALITY as u32,
        0,
        wide("Segoe UI").as_ptr(),
    );
    let old = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, color);
    let value = wide(value);
    TextOutW(dc, x, y, value.as_ptr(), (value.len() - 1) as i32);
    SelectObject(dc, old);
    DeleteObject(font);
}
unsafe fn paint(dc: HDC, frame: &Frame) {
    let s = frame.scale;
    let px = |v: f64| (v * s).round() as i32;
    let point = |p: [f64; 2]| {
        [
            (p[0] - frame.bounds[0] as f64).round() as i32,
            (p[1] - frame.bounds[1] as f64).round() as i32,
        ]
    };
    let e = &frame.event;
    let seconds = frame.start.elapsed().as_secs_f64();
    let landed = point(e.landed);
    ring(dc, landed, px(10.), 0, false, px(4.).max(1));
    ring(dc, landed, px(10.), ORANGE, false, px(2.).max(1));
    if let Some(selection) = e.selection {
        let clicked = point(selection);
        let progress = ease(seconds / 0.35);
        let to: [f64; 2] =
            std::array::from_fn(|i| e.landed[i] + (selection[i] - e.landed[i]) * progress);
        let to = point(to);
        let pen = CreatePen(PS_DOT, 1, WHITE);
        let old = SelectObject(dc, pen);
        MoveToEx(dc, landed[0], landed[1], null_mut());
        LineTo(dc, to[0], to[1]);
        SelectObject(dc, old);
        DeleteObject(pen);
        ring(
            dc,
            clicked,
            px(6. + 4. * (1. - ease(seconds / 0.4))),
            0,
            false,
            px(4.).max(1),
        );
        ring(
            dc,
            clicked,
            px(6. + 4. * (1. - ease(seconds / 0.4))),
            WHITE,
            false,
            px(2.).max(1),
        );
    }
    if e.updated && seconds >= 0.4 {
        let p = point(animated_point(e, seconds));
        ring(dc, p, px(5.), 0, true, 1);
        ring(dc, p, px(3.5), MINT, true, 1);
    }
    // Keep the label away from the click, including at the display's bottom edge.
    let (title, detail) = caption(e);
    let anchor = point(e.selection.unwrap_or(e.landed));
    let top = if anchor[1] > frame.bounds[3] * 3 / 5 {
        px(2.)
    } else {
        frame.bounds[3] - px(58.)
    };
    let brush = CreateSolidBrush(0x00251c15);
    let pen = CreatePen(PS_SOLID, 1, 0x0054483b);
    let old_b = SelectObject(dc, brush);
    let old_p = SelectObject(dc, pen);
    RoundRect(
        dc,
        px(2.),
        top,
        frame.bounds[2] - px(2.),
        top + px(56.),
        px(12.),
        px(12.),
    );
    SelectObject(dc, old_b);
    SelectObject(dc, old_p);
    DeleteObject(brush);
    DeleteObject(pen);
    text(
        dc,
        px(12.),
        top + px(8.),
        px(16.),
        if e.updated { MINT } else { ORANGE },
        &title,
    );
    text(dc, px(12.), top + px(32.), px(12.), WHITE, &detail);
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rect: RECT = zeroed();
            GetClientRect(hwnd, &mut rect);
            let buffer = CreateCompatibleDC(dc);
            let bitmap = CreateCompatibleBitmap(dc, rect.right.max(1), rect.bottom.max(1));
            if !buffer.is_null() && !bitmap.is_null() {
                let old = SelectObject(buffer, bitmap);
                let brush = CreateSolidBrush(KEY);
                FillRect(buffer, &rect, brush);
                DeleteObject(brush);
                let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
                if !state.is_null() {
                    if let Some(frame) = &(*state).frame {
                        paint(buffer, frame);
                    }
                }
                BitBlt(dc, 0, 0, rect.right, rect.bottom, buffer, 0, 0, SRCCOPY);
                SelectObject(buffer, old);
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !buffer.is_null() {
                DeleteDC(buffer);
            }
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Feedback {
        Feedback {
            landed: [-100., 400.],
            before: [-100., 400.],
            after: [-98., 401.],
            selection: Some([-70., 420.]),
            rect: [-1920, 0, 0, 1080],
            accepted: true,
            updated: true,
            reason: "Learning local correction map",
            demo: false,
        }
    }
    #[test]
    fn animation_ends_at_actual_small_adjustment_not_the_clicked_target() {
        let e = event();
        assert_eq!(animated_point(&e, 0.), e.before);
        assert_eq!(animated_point(&e, 2.), e.after);
        for i in 0..30 {
            let p = animated_point(&e, i as f64 / 10.);
            assert!((-100. ..=-98.).contains(&p[0]));
        }
        assert!(caption(&e).0.contains("2.2 px"));
        let mut unchanged = e;
        unchanged.updated = false;
        unchanged.after = unchanged.before;
        assert_eq!(animated_point(&unchanged, 2.), unchanged.before);
        assert!(caption(&unchanged).0.contains("no map change"));
    }
    #[test]
    fn feedback_fits_negative_origin_and_screen_corners_at_different_dpi() {
        for scale in [1., 1.5, 2.] {
            for p in [[-1919., 1.], [-1., 1079.]] {
                let mut e = event();
                e.selection = Some(p);
                let b = bounds(&e, scale);
                assert!(
                    b[0] >= e.rect[0]
                        && b[1] >= e.rect[1]
                        && b[0] + b[2] <= e.rect[2]
                        && b[1] + b[3] <= e.rect[3]
                );
            }
        }
    }
}
