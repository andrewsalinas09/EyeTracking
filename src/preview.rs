//! DPI-aware screen mapping: normalized gaze belongs to a whole physical monitor,
//! never to the client rectangle. Moving/resizing the preview must not shift gaze.
use crate::calibration::{Display, Report, Session, CHECK_COUNT, TRAIN_COUNT};
use crate::tobii::{Sample, Worker};
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
mod dashboard;
mod tray;

struct App {
    control_font: HFONT,
    overview: bool,
    tray: Option<tray::Tray>,
    preferences: crate::preferences::Preferences,
    last_ui: Instant,
    mouse: Option<crate::mouse::Controller>,
    worker: Worker,
    trail: bool,
    targets: bool,
    hidden: bool,
    saved: WINDOWPLACEMENT,
    back: Option<Backbuffer>,
    session: Option<Session>,
    report: Option<Report>,
    show_report: bool,
    correction: bool,
    notice: String,
}
impl Drop for App {
    fn drop(&mut self) {
        if !self.control_font.is_null() {
            unsafe {
                DeleteObject(self.control_font);
            }
        }
    }
}
thread_local! { static APP: RefCell<Option<App>> = const { RefCell::new(None) }; }
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

fn map_point(xy: [f32; 2], monitor: RECT, origin: POINT) -> [f32; 2] {
    [
        monitor.left as f32 + xy[0] * (monitor.right - monitor.left) as f32 - origin.x as f32,
        monitor.top as f32 + xy[1] * (monitor.bottom - monitor.top) as f32 - origin.y as f32,
    ]
}
fn is_live(sample: &Sample, now: Instant) -> bool {
    sample.valid && now.duration_since(sample.received) <= Duration::from_millis(200)
}
/// Keep all calibration instructions/buttons opposite the active target.
/// Logical pixels, shared by painting and child-control placement.
fn calibration_panel(w: f32, h: f32, upper_target: bool) -> [f32; 4] {
    let width = 760_f32.min(w - 48.);
    [
        (w - width) / 2.,
        if upper_target { h - 232. } else { 24. },
        width,
        208.,
    ]
}

unsafe fn display(hwnd: HWND) -> Display {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFOEXW = zeroed();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO);
    let r = info.monitorInfo.rcMonitor;
    Display {
        name: String::from_utf16_lossy(
            &info.szDevice[..info.szDevice.iter().position(|c| *c == 0).unwrap_or(32)],
        ),
        rect: [r.left, r.top, r.right, r.bottom],
    }
}
unsafe fn update_calibration(hwnd: HWND, app: &mut App) {
    let current = display(hwnd);
    if let Some(session) = app.session.as_mut() {
        if session.display != current {
            app.session = None;
            if let Some(mouse) = &app.mouse {
                mouse.calibration(false, None);
            }
            app.notice =
                "Calibration cancelled because the display changed. Select Calibrate to restart."
                    .into();
            return;
        }
        if GetForegroundWindow() != hwnd {
            session.pause();
            return;
        }
        let state = app.worker.state.lock().unwrap();
        if state.status != "Connected" {
            session.pause();
            return;
        }
        let samples: Vec<_> = state.samples.iter().copied().collect();
        drop(state);
        session.tick(&samples, Instant::now());
        if let Some(report) = session.finished.take() {
            app.correction = report.metrics.recommend;
            app.notice=match report.save(){Ok(_)=>"Session saved locally in recordings. R switches between the map and live preview.".into(),Err(e)=>format!("Could not save session: {e}")};
            app.report = Some(report);
            if let Some(mouse) = &app.mouse {
                mouse.calibration(false, app.report.as_ref());
            }
            app.show_report = true;
            app.session = None;
        }
    }
}
fn corrected(
    report: Option<&Report>,
    enabled: bool,
    point: [f32; 2],
    current: &Display,
) -> [f32; 2] {
    if enabled {
        if let Some(report) = report.filter(|r| r.display == *current) {
            return report
                .model
                .apply([point[0] as f64, point[1] as f64])
                .map(|x| x as f32);
        }
    }
    point
}
fn line(pm: &mut Pixmap, a: [f32; 2], b: [f32; 2], color: [u8; 4], width: f32) {
    let mut path = PathBuilder::new();
    path.move_to(a[0], a[1]);
    path.line_to(b[0], b[1]);
    if let Some(path) = path.finish() {
        let mut paint = Paint::default();
        paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
        pm.stroke_path(
            &path,
            &paint,
            &Stroke {
                width,
                ..Stroke::default()
            },
            Transform::identity(),
            None,
        );
    }
}
fn draw_session(pm: &mut Pixmap, session: &Session, area: RECT, origin: POINT, scale: f32) {
    for y in [0.18, 0.5, 0.82] {
        for x in [0.1, 0.3, 0.5, 0.7, 0.9] {
            let p = map_point([x, y], area, origin);
            circle(pm, p[0], p[1], 2.0 * scale, [60, 73, 90, 255], None);
        }
    }
    for capture in &session.captures {
        let p = map_point(capture.target.map(|x| x as f32), area, origin);
        circle(pm, p[0], p[1], 4.0 * scale, [67, 134, 129, 255], None);
    }
    let p = map_point(session.target().map(|x| x as f32), area, origin);
    let color = if session.capturing() {
        [96, 232, 229, 255]
    } else {
        [248, 200, 104, 255]
    };
    circle(pm, p[0], p[1], 24.0 * scale, color, Some(1.5 * scale));
    circle(pm, p[0], p[1], 3.0 * scale, [244, 250, 255, 255], None);
    if session.capturing() {
        circle(
            pm,
            p[0],
            p[1],
            (30.0 + 10.0 * session.progress(Instant::now())) * scale,
            [96, 232, 229, 140],
            Some(2.0 * scale),
        );
    }
}
fn draw_report(pm: &mut Pixmap, report: &Report, area: RECT, origin: POINT, scale: f32) {
    for cap in &report.training {
        let target = map_point(cap.target.map(|v| v as f32), area, origin);
        for sample in &cap.retained {
            let p = map_point(sample.map(|v| v as f32), area, origin);
            circle(pm, p[0], p[1], 1.5 * scale, [131, 154, 176, 65], None);
        }
        for sample in &cap.rejected {
            let p = map_point(sample.map(|v| v as f32), area, origin);
            circle(pm, p[0], p[1], 2.0 * scale, [235, 90, 100, 120], None);
        }
        circle(
            pm,
            target[0],
            target[1],
            4.0 * scale,
            [112, 136, 157, 210],
            Some(scale),
        );
    }
    for cap in &report.validation {
        let target = map_point(cap.target.map(|v| v as f32), area, origin);
        let raw = map_point(cap.mean.map(|v| v as f32), area, origin);
        let corrected = map_point(report.model.apply(cap.mean).map(|v| v as f32), area, origin);
        line(pm, raw, target, [248, 181, 99, 160], scale);
        line(pm, corrected, target, [96, 232, 229, 210], scale);
        circle(
            pm,
            raw[0],
            raw[1],
            7.0 * scale,
            [248, 181, 99, 255],
            Some(2.0 * scale),
        );
        circle(
            pm,
            corrected[0],
            corrected[1],
            5.0 * scale,
            [96, 232, 229, 255],
            None,
        );
        circle(
            pm,
            target[0],
            target[1],
            12.0 * scale,
            [234, 243, 250, 255],
            Some(scale),
        );
        circle(
            pm,
            target[0],
            target[1],
            2.0 * scale,
            [234, 243, 250, 255],
            None,
        );
    }
}

struct Backbuffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    pixmap: Pixmap,
}
impl Backbuffer {
    unsafe fn new(hdc: HDC, w: i32, h: i32) -> Option<Self> {
        let pixmap = Pixmap::new(w as u32, h as u32)?;
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() {
            return None;
        }
        let dc = CreateCompatibleDC(hdc);
        if dc.is_null() {
            DeleteObject(bitmap);
            return None;
        }
        let previous = SelectObject(dc, bitmap);
        Some(Self {
            dc,
            bitmap,
            previous,
            bits: bits as *mut u8,
            pixmap,
        })
    }
}
impl Drop for Backbuffer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}
fn circle(pm: &mut Pixmap, x: f32, y: f32, r: f32, rgba: [u8; 4], stroke: Option<f32>) {
    if let Some(path) = PathBuilder::from_circle(x, y, r) {
        let mut paint = Paint::default();
        paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
        if let Some(width) = stroke {
            pm.stroke_path(
                &path,
                &paint,
                &Stroke {
                    width,
                    ..Stroke::default()
                },
                Transform::identity(),
                None,
            );
        } else {
            pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}
unsafe fn label(dc: HDC, x: i32, y: i32, height: i32, color: u32, text: &str) {
    let font = CreateFontW(
        -height,
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
        CLEARTYPE_QUALITY as u32,
        0,
        wide("Segoe UI").as_ptr(),
    );
    let old = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, color);
    let text = wide(text);
    TextOutW(dc, x, y, text.as_ptr(), (text.len() - 1) as i32);
    SelectObject(dc, old);
    DeleteObject(font);
}

unsafe fn paint(hwnd: HWND, app: &mut App) {
    let mut ps: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut client: RECT = zeroed();
    GetClientRect(hwnd, &mut client);
    let w = client.right;
    let h = client.bottom;
    if w <= 0 || h <= 0 {
        EndPaint(hwnd, &ps);
        return;
    }
    if app
        .back
        .as_ref()
        .is_none_or(|b| b.pixmap.width() != w as u32 || b.pixmap.height() != h as u32)
    {
        app.back = Backbuffer::new(dc, w, h);
    }
    let Some(back) = app.back.as_mut() else {
        EndPaint(hwnd, &ps);
        return;
    };
    let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
    let px = |n: f32| (n * scale).round() as i32;
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFOEXW = zeroed();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO);
    let area = info.monitorInfo.rcMonitor;
    let name = String::from_utf16_lossy(
        &info.szDevice[..info.szDevice.iter().position(|c| *c == 0).unwrap_or(32)],
    );
    let current = Display {
        name: name.clone(),
        rect: [area.left, area.top, area.right, area.bottom],
    };
    let report_visible = app.show_report
        && app.report.as_ref().is_some_and(|r| r.display == current)
        && app.session.is_none();
    let correction_active = app.correction
        && app.report.as_ref().is_some_and(|r| r.display == current)
        && app.session.is_none();
    let live_position = |point| {
        let base = corrected(app.report.as_ref(), correction_active, point, &current);
        let learned = app
            .mouse
            .as_ref()
            .map_or([0.0; 2], |mouse| mouse.offset(&current, base));
        [base[0] + learned[0], base[1] + learned[1]]
    };
    let mut origin = POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut origin);
    let now = Instant::now();
    let (status, hz, samples, version) = {
        let state = app.worker.state.lock().unwrap();
        (
            state.status.clone(),
            state.hz(),
            state
                .samples
                .iter()
                .copied()
                .filter(|s| now.duration_since(s.received) < Duration::from_millis(600))
                .collect::<Vec<_>>(),
            state.version.clone(),
        )
    };
    let latest = samples.last();
    let live = latest.filter(|s| status == "Connected" && is_live(s, now));
    back.pixmap.fill(Color::from_rgba8(12, 20, 30, 255));
    if app.targets && app.session.is_none() && !report_visible {
        for ny in [0.25, 0.5, 0.75] {
            for nx in [0.2, 0.5, 0.8] {
                let x = nx * w as f32;
                let y = px(115.0) as f32 + ny * (h - px(215.0)).max(0) as f32;
                circle(
                    &mut back.pixmap,
                    x,
                    y,
                    18.0 * scale,
                    [59, 76, 94, 255],
                    Some(scale),
                );
                circle(
                    &mut back.pixmap,
                    x,
                    y,
                    2.5 * scale,
                    [142, 160, 180, 255],
                    None,
                );
            }
        }
    }
    if !app.hidden && app.session.is_none() && !report_visible {
        if app.trail && live.is_some() {
            for sample in samples.iter().filter(|s| s.valid) {
                let age = now.duration_since(sample.received).as_secs_f32() / 0.6;
                let p = map_point(live_position(sample.xy), area, origin);
                circle(
                    &mut back.pixmap,
                    p[0],
                    p[1],
                    3.0 * scale,
                    [79, 213, 221, ((1.0 - age) * 95.0) as u8],
                    None,
                );
            }
        }
        if let Some(sample) = live {
            let p = map_point(live_position(sample.xy), area, origin);
            circle(
                &mut back.pixmap,
                p[0],
                p[1],
                32.0 * scale,
                [79, 222, 223, 18],
                None,
            );
            circle(
                &mut back.pixmap,
                p[0],
                p[1],
                15.0 * scale,
                [96, 232, 229, 235],
                Some(2.0 * scale),
            );
            circle(
                &mut back.pixmap,
                p[0],
                p[1],
                3.5 * scale,
                [212, 255, 248, 255],
                None,
            );
        }
    }
    if let Some(session) = app.session.as_ref() {
        draw_session(&mut back.pixmap, session, area, origin, scale);
    }
    if report_visible {
        draw_report(
            &mut back.pixmap,
            app.report.as_ref().unwrap(),
            area,
            origin,
            scale,
        );
    }
    // tiny-skia RGBA -> Windows DIB BGRA; copy into the persistent backbuffer.
    let target = std::slice::from_raw_parts_mut(back.bits, (w * h * 4) as usize);
    for (dst, src) in target
        .chunks_exact_mut(4)
        .zip(back.pixmap.data().chunks_exact(4))
    {
        dst.copy_from_slice(&[src[2], src[1], src[0], 255]);
    }
    let dim = rgb(134, 154, 174);
    let white = rgb(227, 238, 245);
    let teal = rgb(96, 232, 229);
    if let Some(session) = app.session.as_ref() {
        let count = session.captures.len();
        let heading = if count < TRAIN_COUNT {
            format!("Fit the map · dot {} / {TRAIN_COUNT}", count + 1)
        } else {
            format!(
                "Check the map · dot {} / {CHECK_COUNT}",
                count - TRAIN_COUNT + 1
            )
        };
        let [x, y, width, height] = calibration_panel(
            w as f32 / scale,
            h as f32 / scale,
            session.target()[1] < 0.5,
        );
        let panel = RECT {
            left: px(x),
            top: px(y),
            right: px(x + width),
            bottom: px(y + height),
        };
        let brush = CreateSolidBrush(rgb(18, 29, 40));
        FillRect(back.dc, &panel, brush);
        DeleteObject(brush);
        label(back.dc, px(x + 20.), px(y + 16.), px(26.0), white, &heading);
        label(
            back.dc,
            px(x + 20.),
            px(y + 54.),
            px(14.0),
            dim,
            "Look at the bright dot. Press Space to capture, then keep looking until it moves.",
        );
        label(
            back.dc,
            px(x + 20.),
            px(y + 85.),
            px(14.0),
            teal,
            &session.notice,
        );
        label(
            back.dc,
            px(x + 20.),
            px(y + 114.),
            px(13.0),
            if live.is_some() {
                teal
            } else {
                rgb(248, 181, 99)
            },
            if live.is_some() {
                "Gaze detected · Capture from your normal sitting position · Esc cancels"
            } else {
                "Gaze unavailable here · Keep your normal posture; adjust tracker position before retrying"
            },
        );
    } else if report_visible {
        let report = app.report.as_ref().unwrap();
        let m = &report.metrics;
        label(
            back.dc,
            px(30.0),
            px(25.0),
            px(30.0),
            white,
            &format!("{} correction · independent check", report.model.kind),
        );
        label(
            back.dc,
            px(30.0),
            px(70.0),
            px(20.0),
            teal,
            &format!(
                "Mean target error: {:.1} → {:.1} px     ·     {} / {} dots improved",
                m.raw_mean_px,
                m.corrected_mean_px,
                m.improved_targets,
                report.validation.len()
            ),
        );
        label(
            back.dc,
            px(30.0),
            px(102.0),
            px(15.0),
            dim,
            &format!(
                "All-valid-sample RMS: {:.1} → {:.1} px    ·    Worst target: {:.1} → {:.1} px",
                m.raw_sample_rms_px,
                m.corrected_sample_rms_px,
                m.raw_worst_target_px,
                m.corrected_worst_target_px
            ),
        );
        if m.corner_errors.is_empty() {
            label(back.dc,px(30.),h-px(213.),px(14.),rgb(248,181,99),"Corners were not checked in this older calibration. Recalibrate for full-screen coverage.");
        } else {
            for (row, names) in [["Upper left", "Upper right"], ["Lower left", "Lower right"]]
                .iter()
                .enumerate()
            {
                let text = names
                    .iter()
                    .filter_map(|name| m.corner_errors.iter().find(|c| c.name == *name))
                    .map(|c| format!("{}: {:.1} → {:.1} px", c.name, c.raw_px, c.corrected_px))
                    .collect::<Vec<_>>()
                    .join("     ·     ");
                label(
                    back.dc,
                    px(30.),
                    h - px(238. - row as f32 * 25.),
                    px(14.),
                    white,
                    &text,
                );
            }
        }
        label(back.dc,px(30.0),h-px(184.0),px(14.0),white,"White: target    Orange: original    Mint: corrected    Grey: fitting samples    Red: outliers");
        label(
            back.dc,
            px(30.0),
            h - px(154.0),
            px(17.0),
            if m.recommend { teal } else { rgb(248, 181, 99) },
            &format!(
                "{}  Correction {}.",
                if m.recommend {
                    "Improvement passed the checks."
                } else {
                    "No reliable improvement; keep the original mapping."
                },
                if correction_active { "ON" } else { "OFF" }
            ),
        );
        label(back.dc, px(30.0), h - px(124.0), px(13.0), dim, &app.notice);
    } else {
        label(back.dc, px(30.0), px(25.0), px(30.0), white, "Gaze preview");
        label(
            back.dc,
            px(31.0),
            px(68.0),
            px(15.0),
            dim,
            "Explore your gaze across the screen, or select Calibrate to measure its accuracy.",
        );
        let message = if status != "Connected" {
            status.clone()
        } else if live.is_none() {
            "Waiting for eyes — face the tracker".into()
        } else {
            let sample = live.unwrap();
            let p = map_point(live_position(sample.xy), area, origin);
            if sample.xy.iter().any(|v| *v < 0.0 || *v > 1.0) {
                "Tracking · gaze is outside this display".into()
            } else if p[0] < 0.0 || p[1] < 0.0 || p[0] >= w as f32 || p[1] >= h as f32 {
                "Tracking · gaze is outside this window. Select Full screen to see the whole display.".into()
            } else if app.hidden {
                "Tracking · gaze marker hidden".into()
            } else {
                format!(
                    "Tracking · correction {}",
                    if correction_active { "ON" } else { "OFF" }
                )
            }
        };
        label(
            back.dc,
            px(30.0),
            h - px(161.0),
            px(17.0),
            if live.is_some() {
                teal
            } else {
                rgb(232, 188, 110)
            },
            &message,
        );
        label(
            back.dc,
            px(30.0),
            h - px(132.0),
            px(14.0),
            dim,
            &format!(
                "{hz:.1} Hz   ·   {name}   {} × {}   ·   Stream Engine {version}",
                area.right - area.left,
                area.bottom - area.top
            ),
        );
        if !app.notice.is_empty() {
            label(back.dc, px(30.0), px(98.0), px(14.0), dim, &app.notice);
        }
    }
    BitBlt(dc, 0, 0, w, h, back.dc, 0, 0, SRCCOPY);
    EndPaint(hwnd, &ps);
}

unsafe fn toggle_fullscreen(hwnd: HWND) {
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_OVERLAPPEDWINDOW != 0 {
        let mut saved: WINDOWPLACEMENT = zeroed();
        saved.length = size_of::<WINDOWPLACEMENT>() as u32;
        GetWindowPlacement(hwnd, &mut saved);
        APP.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                a.saved = saved;
            }
        });
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
        SetWindowLongW(
            hwnd,
            GWL_STYLE,
            (WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN) as i32,
        );
        let r = info.rcMonitor;
        SetWindowPos(
            hwnd,
            null_mut(),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_FRAMECHANGED | SWP_NOZORDER,
        );
    } else {
        let saved = APP.with(|a| a.borrow().as_ref().map(|a| a.saved));
        SetWindowLongW(
            hwnd,
            GWL_STYLE,
            (WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPCHILDREN) as i32,
        );
        if let Some(saved) = saved {
            SetWindowPlacement(hwnd, &saved);
        }
        SetWindowPos(
            hwnd,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        );
    }
}
unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    (*(data as *mut Vec<HMONITOR>)).push(monitor);
    1
}
unsafe fn next_monitor(hwnd: HWND) {
    let mut monitors = Vec::<HMONITOR>::new();
    EnumDisplayMonitors(
        null_mut(),
        null(),
        Some(enum_monitor),
        &mut monitors as *mut _ as isize,
    );
    if monitors.len() < 2 {
        return;
    }
    let current = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let next =
        monitors[(monitors.iter().position(|m| *m == current).unwrap_or(0) + 1) % monitors.len()];
    let full = GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_OVERLAPPEDWINDOW == 0;
    let mut info: MONITORINFO = zeroed();
    info.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(next, &mut info);
    if !full {
        ShowWindow(hwnd, SW_RESTORE);
    }
    let r = if full { info.rcMonitor } else { info.rcWork };
    SetWindowPos(
        hwnd,
        null_mut(),
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        SWP_NOZORDER,
    );
    if !full {
        ShowWindow(hwnd, SW_MAXIMIZE);
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let restart = msg >= 0xc000
        && APP.with(|a| {
            a.borrow()
                .as_ref()
                .and_then(|a| a.tray.as_ref())
                .is_some_and(|t| t.restart_message != 0 && msg == t.restart_message)
        });
    if restart {
        let available = APP.with(|a| {
            let mut a = a.borrow_mut();
            if let Some(t) = a.as_mut().and_then(|a| a.tray.as_mut()) {
                t.add();
                t.available
            } else {
                false
            }
        });
        if !available {
            dashboard::home(hwnd);
        }
        return 0;
    }
    match msg {
        tray::OPEN => {
            dashboard::home(hwnd);
            0
        }
        tray::CALLBACK => {
            tray::callback(hwnd, w, l);
            0
        }
        WM_CLOSE => {
            dashboard::hide(hwnd);
            0
        }
        WM_COMMAND => {
            dashboard::command(hwnd, (w & 0xffff) as u32);
            0
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => dashboard::edit_colors(w as HDC),
        WM_DRAWITEM => {
            dashboard::draw_button(
                &*(l as *const windows_sys::Win32::UI::Controls::DRAWITEMSTRUCT),
            );
            1
        }
        WM_SIZE => {
            dashboard::layout(hwnd);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_CREATE => {
            SetTimer(hwnd, 1, 16, None);
            0
        }
        WM_TIMER => {
            let (repaint, refresh, layout) = APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    let session = app.session.as_ref().map(|s| s.captures.len());
                    update_calibration(hwnd, app);
                    let refresh = app.last_ui.elapsed() >= Duration::from_millis(250);
                    if refresh {
                        app.last_ui = Instant::now();
                        if let Some(mouse) = &app.mouse {
                            let s = mouse.snapshot();
                            let prefs = crate::preferences::Preferences {
                                enabled: s.enabled,
                                dot: s.dot,
                                scroll: s.scroll,
                                learning: s.learning,
                                mouse_rearm_ms: s.mouse_rearm_ms,
                                trackpad_rearm_ms: s.trackpad_rearm_ms,
                            };
                            if prefs != app.preferences {
                                match prefs.save() {
                                    Ok(()) => app.preferences = prefs,
                                    Err(e) => {
                                        app.notice = format!("Could not save preferences: {e}")
                                    }
                                }
                            }
                            if let Some(t) = app.tray.as_mut() {
                                let state = app.worker.state.lock().unwrap();
                                let live = state
                                    .samples
                                    .back()
                                    .is_some_and(|p| is_live(p, Instant::now()));
                                t.set_tip(if !s.enabled {
                                    "EyeTracking — paused"
                                } else if !s.display_ok {
                                    "EyeTracking — display setup changed"
                                } else if !live {
                                    "EyeTracking — waiting for gaze"
                                } else {
                                    "EyeTracking — gaze control active"
                                });
                            }
                        }
                    }
                    (
                        !app.overview || refresh,
                        refresh,
                        session != app.session.as_ref().map(|s| s.captures.len()),
                    )
                } else {
                    (false, false, false)
                }
            });
            if layout {
                dashboard::layout(hwnd);
            }
            if refresh {
                dashboard::refresh(hwnd);
            }
            if repaint && IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 {
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    if app.overview {
                        dashboard::paint(hwnd, app);
                    } else {
                        paint(hwnd, app);
                    }
                }
            });
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(l as *mut MINMAXINFO);
            let scale = GetDpiForWindow(hwnd) as f32 / 96.;
            info.ptMinTrackSize = POINT {
                x: (960. * scale) as i32,
                y: (760. * scale) as i32,
            };
            0
        }
        WM_DPICHANGED => {
            let r = *(l as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER,
            );
            dashboard::fonts(hwnd);
            dashboard::layout(hwnd);
            0
        }
        WM_KEYDOWN => {
            if l & (1 << 30) != 0 {
                return 0;
            }
            let in_session = APP.with(|a| a.borrow().as_ref().is_some_and(|a| a.session.is_some()));
            if in_session {
                APP.with(|a| {
                    if let Some(a) = a.borrow_mut().as_mut() {
                        match w as u32 {
                            0x20 => {
                                if let Some(session) = a.session.as_mut() {
                                    session.start_capture();
                                }
                            }
                            0x1B => {
                                a.session = None;
                                if let Some(mouse) = &a.mouse {
                                    mouse.calibration(false, None);
                                }
                                a.notice =
                                    "Calibration cancelled. Select Calibrate to start again."
                                        .into();
                            }
                            _ => {}
                        }
                    }
                });
                dashboard::layout(hwnd);
                return 0;
            }
            match w as u32 {
                0x1B => {
                    dashboard::home(hwnd);
                }
                0x43 => {
                    if GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_OVERLAPPEDWINDOW != 0 {
                        toggle_fullscreen(hwnd);
                    }
                    let current = display(hwnd);
                    APP.with(|a| {
                        if let Some(a) = a.borrow_mut().as_mut() {
                            a.overview = false;
                            a.session = Some(Session::new(current));
                            if let Some(mouse) = &a.mouse {
                                mouse.calibration(true, None);
                            }
                            a.show_report = false;
                            a.notice.clear();
                        }
                    });
                }
                0x7A => toggle_fullscreen(hwnd),
                0x4D => next_monitor(hwnd),
                _ => APP.with(|a| {
                    if let Some(a) = a.borrow_mut().as_mut() {
                        match w as u32 {
                            0x57 => {
                                if let Some(mouse) = &a.mouse {
                                    mouse.toggle();
                                }
                            }
                            0x4C => {
                                a.notice = match a.mouse.as_ref().map(|m| m.open_learning_map()) {
                                    Some(Ok(())) => "Opening saved learning map in your browser. L refreshes the snapshot.".into(),
                                    Some(Err(e)) => e,
                                    None => "Mouse controller is unavailable.".into(),
                                };
                            }
                            0x54 => a.trail = !a.trail,
                            0x47 => a.targets = !a.targets,
                            0x20 => a.hidden = !a.hidden,
                            0x52 if a.report.is_some() => {
                                a.show_report = !a.show_report;
                            }
                            0x41 if a.report.is_some() => {
                                a.correction = !a.correction;
                                if let (Some(mouse), Some(report)) = (&a.mouse, &a.report) {
                                    mouse.correction(report, a.correction);
                                }
                            }
                            _ => {}
                        }
                    }
                }),
            }
            dashboard::layout(hwnd);
            dashboard::refresh(hwnd);
            0
        }
        WM_DESTROY => {
            APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    a.tray.take();
                }
            });
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        let class = wide("EyeTrackingGazePreview");
        let existing = FindWindowW(class.as_ptr(), null());
        if !existing.is_null() {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
            PostMessageW(existing, tray::OPEN, 0, 0);
            return;
        }
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let report = Report::load_latest();
        let correction = report.as_ref().is_some_and(|r| r.metrics.recommend);
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                control_font: null_mut(),
                overview: true,
                tray: None,
                preferences: crate::preferences::Preferences::load(),
                last_ui: Instant::now(),
                mouse: None,
                worker: Worker::start(),
                trail: true,
                targets: true,
                hidden: false,
                saved: zeroed(),
                back: None,
                session: None,
                report,
                show_report: false,
                correction,
                notice: String::new(),
            })
        });
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("EyeTracking").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1040,
            800,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            MessageBoxW(
                null_mut(),
                wide("Could not create the gaze preview window.").as_ptr(),
                wide("EyeTracking").as_ptr(),
                MB_ICONERROR,
            );
        } else {
            let dark: BOOL = 1;
            windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                hwnd,
                windows_sys::Win32::Graphics::Dwm::DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                &dark as *const _ as *const _,
                size_of::<BOOL>() as u32,
            );
            APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    match crate::mouse::Controller::start(
                        a.worker.state.clone(),
                        display(hwnd),
                        a.report.as_ref(),
                        a.preferences.enabled || std::env::args().any(|x| x == "--mouse"),
                    ) {
                        Ok(mouse) => {
                            mouse.set_rearm_delay(a.preferences.mouse_rearm_ms);
                            mouse.set_trackpad_rearm_delay(a.preferences.trackpad_rearm_ms);
                            if !a.preferences.dot {
                                mouse.command(4);
                            }
                            if !a.preferences.scroll {
                                mouse.command(6);
                            }
                            if !a.preferences.learning {
                                mouse.command(2);
                            }
                            a.mouse = Some(mouse);
                        }
                        Err(error) => a.notice = error,
                    }
                }
            });
            let tray = tray::Tray::new(hwnd);
            APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    a.tray = Some(tray);
                }
            });
            dashboard::create(hwnd);
            ShowWindow(hwnd, SW_SHOWNORMAL);
            SetForegroundWindow(hwnd);
            let mut msg: MSG = zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                if msg.message == WM_KEYDOWN && msg.wParam == 0x1b {
                    dashboard::home(hwnd);
                    continue;
                }
                if IsDialogMessageW(hwnd, &msg) != 0 {
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        APP.with(|a| {
            a.borrow_mut().take();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibration_chrome_stays_clear_of_edge_and_interior_targets() {
        for (w, h) in [(960., 720.), (1920., 1080.), (3840., 2160.)] {
            for y in [0.04, 0.07, 0.18, 0.3, 0.5, 0.7, 0.82, 0.93, 0.96] {
                let [_, top, _, height] = calibration_panel(w, h, y < 0.5);
                assert!(y * h + 24. < top || y * h - 24. > top + height);
                assert!(top >= 0. && top + height <= h);
            }
        }
    }
    #[test]
    fn screen_mapping_handles_negative_monitor_origin_and_window_offset() {
        let monitor = RECT {
            left: -2560,
            top: 100,
            right: 0,
            bottom: 1540,
        };
        assert_eq!(
            map_point([0.5, 0.5], monitor, POINT { x: -2400, y: 200 }),
            [1120.0, 620.0]
        );
        assert_eq!(
            map_point([0.0, 0.0], monitor, POINT { x: -2400, y: 200 }),
            [-160.0, -100.0]
        );
    }
    #[test]
    fn stale_and_invalid_samples_do_not_leave_a_live_marker() {
        let now = Instant::now();
        let mut sample = Sample {
            xy: [0.5, 0.5],
            valid: true,
            timestamp_us: 0,
            received: now,
        };
        assert!(is_live(&sample, now));
        sample.valid = false;
        assert!(!is_live(&sample, now));
        sample.valid = true;
        sample.received = now - Duration::from_millis(201);
        assert!(!is_live(&sample, now));
    }
}
