//! Native, keyboard-accessible controls; painting never runs on the input thread.
use super::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};

pub const HOME: u32 = 101;
pub const PREVIEW: u32 = 102;
pub const RESULTS: u32 = 103;
pub const POWER: u32 = 104;
pub const DOT: u32 = 105;
pub const SCROLL: u32 = 106;
pub const LEARN: u32 = 107;
pub const CALIBRATE: u32 = 108;
pub const HISTORY: u32 = 109;
pub const RESET: u32 = 110;
pub const HIDE: u32 = 111;
pub const QUIT: u32 = 112;
pub const CAPTURE: u32 = 113;
pub const CANCEL: u32 = 114;
pub const FULLSCREEN: u32 = 115;
pub const DISPLAY: u32 = 116;
pub const CORRECTION: u32 = 117;
pub const TRAIL: u32 = 118;
pub const TARGETS: u32 = 119;
const REARM_LABEL: u32 = 120;
const REARM_EDIT: u32 = 121;
const REARM_APPLY: u32 = 122;
const TRACKPAD_LABEL: u32 = 123;
const TRACKPAD_EDIT: u32 = 124;
const REFIT: u32 = 125;
const FEEDBACK: u32 = 126;
const FEEDBACK_DEMO: u32 = 127;
const ALL: &[u32] = &[
    HOME,
    PREVIEW,
    RESULTS,
    POWER,
    DOT,
    SCROLL,
    LEARN,
    CALIBRATE,
    HISTORY,
    RESET,
    HIDE,
    QUIT,
    CAPTURE,
    CANCEL,
    FULLSCREEN,
    DISPLAY,
    CORRECTION,
    TRAIL,
    TARGETS,
    REARM_LABEL,
    REARM_EDIT,
    TRACKPAD_LABEL,
    TRACKPAD_EDIT,
    REARM_APPLY,
    REFIT,
    FEEDBACK,
    FEEDBACK_DEMO,
];
const BG: u32 = 0x211913;
const CARD: u32 = 0x2e251d;
const EDGE: u32 = 0x44392d;
const TEXT: u32 = 0xf4f4ec;
const MUTED: u32 = 0xb5a899;
const MINT: u32 = 0xc2e669;

pub unsafe fn create(hwnd: HWND) {
    for &id in ALL {
        let (class, style) = match id {
            REARM_LABEL | TRACKPAD_LABEL => ("STATIC", 0), // SS_LEFT is the default static style.
            REARM_EDIT | TRACKPAD_EDIT => (
                "EDIT",
                WS_TABSTOP | WS_BORDER | ES_NUMBER as u32 | ES_CENTER as u32,
            ),
            _ => ("BUTTON", WS_TABSTOP | BS_OWNERDRAW as u32),
        };
        let control = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide("").as_ptr(),
            WS_CHILD | style,
            0,
            0,
            0,
            0,
            hwnd,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        if id == REARM_EDIT || id == REARM_LABEL {
            SendMessageW(
                control,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                0,
            );
        }
    }
    fonts(hwnd);
    let delay = APP.with(|a| a.borrow().as_ref().unwrap().preferences.mouse_rearm_ms);
    SetWindowTextW(
        GetDlgItem(hwnd, REARM_EDIT as i32),
        wide(&delay.to_string()).as_ptr(),
    );
    SendMessageW(GetDlgItem(hwnd, REARM_EDIT as i32), EM_SETLIMITTEXT, 4, 0);
    let delay = APP.with(|a| a.borrow().as_ref().unwrap().preferences.trackpad_rearm_ms);
    SetWindowTextW(
        GetDlgItem(hwnd, TRACKPAD_EDIT as i32),
        wide(&delay.to_string()).as_ptr(),
    );
    SendMessageW(
        GetDlgItem(hwnd, TRACKPAD_EDIT as i32),
        EM_SETLIMITTEXT,
        4,
        0,
    );
    refresh(hwnd);
    layout(hwnd);
}
pub unsafe fn fonts(hwnd: HWND) {
    let height = (14. * GetDpiForWindow(hwnd) as f32 / 96.).round() as i32;
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
    let previous = APP.with(|a| {
        a.borrow_mut()
            .as_mut()
            .map(|a| std::mem::replace(&mut a.control_font, font))
    });
    for id in [REARM_LABEL, REARM_EDIT, TRACKPAD_LABEL, TRACKPAD_EDIT] {
        SendMessageW(GetDlgItem(hwnd, id as i32), WM_SETFONT, font as usize, 1);
    }
    if let Some(previous) = previous {
        if !previous.is_null() {
            DeleteObject(previous);
        }
    }
}
unsafe fn caption(hwnd: HWND, id: u32, text: &str, enabled: bool) {
    let button = GetDlgItem(hwnd, id as i32);
    let mut previous = [0u16; 128];
    let len = GetWindowTextW(button, previous.as_mut_ptr(), previous.len() as i32);
    if String::from_utf16_lossy(&previous[..len as usize]) != text {
        SetWindowTextW(button, wide(text).as_ptr());
        InvalidateRect(button, null(), 0);
    }
    EnableWindow(button, enabled as i32);
}
pub unsafe fn refresh(hwnd: HWND) {
    let visuals = APP.with(|a| {
        a.borrow()
            .as_ref()
            .is_some_and(|a| a.preferences.learning_feedback)
    });
    caption(
        hwnd,
        FEEDBACK,
        if visuals {
            "Learning visuals: On"
        } else {
            "Learning visuals: Off"
        },
        true,
    );
    caption(hwnd, FEEDBACK_DEMO, "Preview animation", visuals);
    let data = APP.with(|a| {
        a.borrow().as_ref().map(|a| {
            (
                a.mouse.as_ref().map(|m| m.snapshot()),
                a.report.is_some(),
                a.correction,
                a.trail,
                a.targets,
            )
        })
    });
    let Some((snapshot, report, correction, trail, targets)) = data else {
        return;
    };
    let available = snapshot.is_some();
    let s = snapshot.unwrap_or_default();
    for (id, text) in [
        (HOME, "Overview"),
        (PREVIEW, "Live preview"),
        (RESULTS, "Calibration results"),
        (CALIBRATE, "Calibrate"),
        (HISTORY, "View learning map"),
        (RESET, "Reset learning"),
        (HIDE, "Hide to tray"),
        (QUIT, "Quit EyeTracking"),
        (CAPTURE, "Capture dot"),
        (CANCEL, "Cancel calibration"),
        (FULLSCREEN, "Full screen"),
        (DISPLAY, "Next display"),
        (REARM_LABEL, "Mouse (ms)"),
        (TRACKPAD_LABEL, "Trackpad (ms)"),
        (REARM_APPLY, "Apply"),
        (REFIT, "Refit saved points"),
    ] {
        caption(
            hwnd,
            id,
            text,
            match id {
                RESULTS | REFIT => report,
                HISTORY | RESET | REARM_APPLY => available,
                _ => true,
            },
        );
    }
    caption(
        hwnd,
        POWER,
        if s.enabled {
            "Pause gaze control"
        } else {
            "Resume gaze control"
        },
        available,
    );
    caption(
        hwnd,
        DOT,
        if s.dot {
            "Gaze dot: On"
        } else {
            "Gaze dot: Off"
        },
        available,
    );
    caption(
        hwnd,
        SCROLL,
        if s.scroll {
            "Gaze scroll: On"
        } else {
            "Gaze scroll: Off"
        },
        available,
    );
    caption(
        hwnd,
        LEARN,
        if s.learning {
            "Learning: On"
        } else {
            "Learning: Frozen"
        },
        available,
    );
    caption(
        hwnd,
        CORRECTION,
        if correction {
            "Correction: On"
        } else {
            "Correction: Off"
        },
        report,
    );
    caption(
        hwnd,
        TRAIL,
        if trail { "Trail: On" } else { "Trail: Off" },
        true,
    );
    caption(
        hwnd,
        TARGETS,
        if targets {
            "Targets: On"
        } else {
            "Targets: Off"
        },
        true,
    );
}
pub unsafe fn layout(hwnd: HWND) {
    let (overview, session) = APP.with(|a| {
        let a = a.borrow();
        let a = a.as_ref().unwrap();
        (a.overview, a.session.as_ref().map(|s| s.target()[1] < 0.5))
    });
    let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
    let px = |n: f32| (n * scale).round() as i32;
    let mut r: RECT = zeroed();
    GetClientRect(hwnd, &mut r);
    let w = r.right as f32 / scale;
    let h = r.bottom as f32 / scale;
    let mut positions = Vec::new();
    if overview {
        positions.extend([
            (HOME, 16., 160., 160., 42.),
            (PREVIEW, 16., 212., 160., 42.),
            (RESULTS, 16., 264., 160., 42.),
            (FEEDBACK, 16., 316., 160., 42.),
            (FEEDBACK_DEMO, 16., 368., 160., 36.),
            (HIDE, 16., h - 112., 160., 40.),
            (QUIT, 16., h - 60., 160., 40.),
            (POWER, w - 220., 150., 180., 44.),
            (DOT, w - 210., 253., 166., 38.),
            (SCROLL, w - 210., 313., 166., 38.),
            (LEARN, w - 210., 373., 166., 38.),
            (REARM_LABEL, w - 352., 426., 96., 22.),
            (REARM_EDIT, w - 352., 450., 86., 26.),
            (TRACKPAD_LABEL, w - 248., 426., 96., 22.),
            (TRACKPAD_EDIT, w - 248., 450., 86., 26.),
            (REARM_APPLY, w - 128., 435., 84., 38.),
        ]);
        let half = (w - 264.) / 2.;
        positions.extend([
            (CALIBRATE, 240., 584., half - 48., 36.),
            (HISTORY, 264. + half, 584., half - 48., 36.),
            (RESET, w - 172., 641., 132., 34.),
        ]);
    } else if let Some(upper_target) = session {
        let [x, y, _, _] = calibration_panel(w, h, upper_target);
        positions.extend([
            (CAPTURE, x + 20., y + 151., 160., 38.),
            (CANCEL, x + 192., y + 151., 184., 38.),
        ]);
    } else {
        positions.extend([
            (HOME, w - 174., 20., 150., 40.),
            (CALIBRATE, 24., h - 94., 130., 38.),
            (RESULTS, 166., h - 94., 170., 38.),
            (CORRECTION, 348., h - 94., 150., 38.),
            (FULLSCREEN, 510., h - 94., 130., 38.),
            (DISPLAY, 652., h - 94., 130., 38.),
            (TRAIL, 24., h - 48., 130., 34.),
            (TARGETS, 166., h - 48., 170., 34.),
            (REFIT, 348., h - 48., 170., 34.),
            (FEEDBACK, 530., h - 48., 170., 34.),
            (FEEDBACK_DEMO, 712., h - 48., 174., 34.),
        ]);
    }
    for &id in ALL {
        let button = GetDlgItem(hwnd, id as i32);
        if let Some(&(_, x, y, width, height)) = positions.iter().find(|p| p.0 == id) {
            SetWindowPos(
                button,
                null_mut(),
                px(x),
                px(y),
                px(width),
                px(height),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        } else {
            ShowWindow(button, SW_HIDE);
        }
    }
}
pub unsafe fn edit_colors(dc: HDC) -> LRESULT {
    SetTextColor(dc, TEXT);
    SetBkColor(dc, CARD);
    SetDCBrushColor(dc, CARD);
    GetStockObject(DC_BRUSH) as LRESULT
}
unsafe fn rounded(dc: HDC, r: RECT, color: u32, border: u32, radius: i32) {
    let brush = CreateSolidBrush(color);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_b = SelectObject(dc, brush);
    let old_p = SelectObject(dc, pen);
    RoundRect(dc, r.left, r.top, r.right, r.bottom, radius, radius);
    SelectObject(dc, old_b);
    SelectObject(dc, old_p);
    DeleteObject(brush);
    DeleteObject(pen);
}
pub unsafe fn draw_button(item: &DRAWITEMSTRUCT) {
    let dc = item.hDC;
    let mut text = [0u16; 128];
    let len = GetWindowTextW(item.hwndItem, text.as_mut_ptr(), 128);
    let name = String::from_utf16_lossy(&text[..len as usize]);
    let selected = item.itemState & ODS_SELECTED != 0;
    let disabled = item.itemState & ODS_DISABLED != 0;
    let active = name.ends_with(": On") || item.CtlID == POWER || item.CtlID == CAPTURE;
    let scale = GetDpiForWindow(item.hwndItem) as f32 / 96.;
    let fill = if selected {
        rgb(58, 107, 95)
    } else if active && !disabled {
        MINT
    } else {
        CARD
    };
    let color = if disabled {
        rgb(105, 116, 123)
    } else if active && !selected {
        BG
    } else {
        TEXT
    };
    let bg = CreateSolidBrush(BG);
    FillRect(dc, &item.rcItem, bg);
    DeleteObject(bg);
    rounded(
        dc,
        item.rcItem,
        fill,
        if active { fill } else { EDGE },
        (12. * scale) as i32,
    );
    let font = CreateFontW(
        -(14. * scale) as i32,
        0,
        0,
        0,
        500,
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
    let mut bounds = item.rcItem;
    DrawTextW(
        dc,
        text.as_ptr(),
        len,
        &mut bounds,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
    );
    SelectObject(dc, old);
    DeleteObject(font);
    if item.itemState & ODS_FOCUS != 0 {
        let mut r = item.rcItem;
        InflateRect(&mut r, -4, -4);
        DrawFocusRect(dc, &r);
    }
}

pub unsafe fn paint(hwnd: HWND, app: &mut App) {
    let mut ps: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut r: RECT = zeroed();
    GetClientRect(hwnd, &mut r);
    if r.right <= 0 || r.bottom <= 0 {
        EndPaint(hwnd, &ps);
        return;
    }
    if app
        .back
        .as_ref()
        .is_none_or(|b| b.pixmap.width() != r.right as u32 || b.pixmap.height() != r.bottom as u32)
    {
        app.back = Backbuffer::new(dc, r.right, r.bottom);
    }
    let Some(back) = app.back.as_mut() else {
        EndPaint(hwnd, &ps);
        return;
    };
    let dc2 = back.dc;
    let s = GetDpiForWindow(hwnd) as f32 / 96.;
    let px = |v: f32| (v * s).round() as i32;
    let w = r.right as f32 / s;
    let h = r.bottom as f32 / s;
    let rect = |x: f32, y: f32, width: f32, height: f32| RECT {
        left: px(x),
        top: px(y),
        right: px(x + width),
        bottom: px(y + height),
    };
    let bg = CreateSolidBrush(BG);
    FillRect(dc2, &r, bg);
    DeleteObject(bg);
    let side = CreateSolidBrush(rgb(15, 23, 30));
    FillRect(dc2, &rect(0., 0., 192., h), side);
    DeleteObject(side);
    let write = |x: f32, y: f32, size: f32, color: u32, text: &str| {
        label(dc2, px(x), px(y), px(size), color, text)
    };
    rounded(dc2, rect(20., 32., 36., 36.), MINT, MINT, px(18.));
    write(30., 35., 25., BG, "◉");
    write(66., 37., 21., TEXT, "EyeTracking");
    write(24., 85., 12., MUTED, "A more natural desktop");
    write(24., 416., 11., MUTED, "Visuals don’t affect learning.");
    write(24., h - 204., 13., TEXT, "Close the panel.");
    write(24., h - 183., 13., TEXT, "Keep the flow.");
    write(24., h - 151., 11., MUTED, "Reopen from the tray icon.");
    write(224., 32., 30., TEXT, "Your eyes, your cursor.");
    write(
        224.,
        77.,
        14.,
        MUTED,
        "Look where you want to go. A small movement takes you there.",
    );

    let state = app.worker.state.lock().unwrap();
    let live = state
        .samples
        .back()
        .is_some_and(|p| is_live(p, Instant::now()));
    let connected = state.status == "Connected";
    let hz = state.hz();
    let connection = state.status.clone();
    drop(state);
    let snapshot = app.mouse.as_ref().map(|m| m.snapshot());
    let available = snapshot.is_some();
    let status = snapshot.unwrap_or_default();
    let (title, detail, color) = if !available {
        (
            "Controller unavailable",
            "Quit and reopen EyeTracking to reconnect input.",
            rgb(255, 177, 112),
        )
    } else if !status.enabled {
        (
            "Gaze control is paused",
            "Resume whenever you’re ready. Your mouse still works normally.",
            rgb(255, 177, 112),
        )
    } else if !status.display_ok {
        (
            "Display setup changed",
            "Restore the calibrated display layout or calibrate this display.",
            rgb(255, 177, 112),
        )
    } else if !live {
        (
            "Waiting for your gaze",
            "Keep your eyes in view of the tracker. It will reconnect automatically.",
            rgb(255, 177, 112),
        )
    } else {
        (
            "You’re in control",
            "Mouse movement or a trackpad slide lands at your gaze.",
            MINT,
        )
    };
    rounded(dc2, rect(224., 116., w - 248., 116.), CARD, EDGE, px(18.));
    write(
        244.,
        130.,
        11.,
        color,
        if status.enabled {
            "GAZE ASSIST"
        } else {
            "PAUSED"
        },
    );
    write(244., 151., 25., TEXT, title);
    write(244., 194., 12., MUTED, detail);

    rounded(dc2, rect(224., 244., w - 248., 247.), CARD, EDGE, px(18.));
    for (y, title, detail) in [
        (
            256.,
            "Desktop gaze dot",
            "Black: ready to jump. Orange: fine control or paused.",
        ),
        (
            316.,
            "Scroll where you look",
            "Look at a window and scroll to focus it.",
        ),
        (
            376.,
            "Learn from small corrections",
            "Short movements followed by clicks refine the local map.",
        ),
    ] {
        write(244., y, 17., TEXT, title);
        write(244., y + 25., 12., MUTED, detail);
    }
    write(244., 436., 17., TEXT, "Jump timing");
    write(
        244.,
        461.,
        12.,
        MUTED,
        "Mouse pause / trackpad repeat delay.",
    );
    let half = (w - 264.) / 2.;
    rounded(dc2, rect(224., 511., half, 123.), CARD, EDGE, px(18.));
    rounded(
        dc2,
        rect(240. + half, 511., half, 123.),
        CARD,
        EDGE,
        px(18.),
    );
    write(244., 522., 19., TEXT, "Make it more accurate");
    let calibration = if app.report.is_some() {
        if app.correction {
            "Saved calibration is applied."
        } else {
            "Saved calibration is available."
        }
    } else {
        "A guided set of dots measures your accuracy."
    };
    write(244., 553., 12., MUTED, calibration);
    write(260. + half, 522., 19., TEXT, "See what it learns");
    write(
        260. + half,
        553.,
        12.,
        MUTED,
        &format!(
            "{} useful clicks  ·  {} map updates",
            status.accepted, status.updates
        ),
    );
    write(260. + half, 572., 11., MUTED, &status.learning_status);
    write(
        224.,
        642.,
        12.,
        if connected { MINT } else { rgb(255, 177, 112) },
        &format!(
            "{}  ·  {:.0} Hz  ·  {} jumps",
            if connected {
                "Tracker connected"
            } else {
                "Tracker reconnecting"
            },
            hz,
            status.jumps
        ),
    );
    let footer = if !app.notice.is_empty() {
        app.notice.clone()
    } else if !status.error.is_empty() {
        status.error.clone()
    } else if !connected {
        connection
    } else {
        format!(
            "{}  ·  Closing this window keeps EyeTracking running.",
            status.display
        )
    };
    // DrawText truncates diagnostics instead of running over controls or off-screen.
    let font = CreateFontW(
        -px(12.),
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
    let old = SelectObject(dc2, font);
    SetBkMode(dc2, TRANSPARENT as i32);
    SetTextColor(dc2, MUTED);
    let mut bounds = rect(224., 681., w - 250., 22.);
    let text = wide(&footer);
    DrawTextW(
        dc2,
        text.as_ptr(),
        (text.len() - 1) as i32,
        &mut bounds,
        DT_SINGLELINE | DT_END_ELLIPSIS,
    );
    SelectObject(dc2, old);
    DeleteObject(font);
    BitBlt(dc, 0, 0, r.right, r.bottom, dc2, 0, 0, SRCCOPY);
    EndPaint(hwnd, &ps);
}

pub unsafe fn home(hwnd: HWND) {
    let full = GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_OVERLAPPEDWINDOW == 0;
    APP.with(|a| {
        if let Some(a) = a.borrow_mut().as_mut() {
            if a.session.take().is_some() {
                if let Some(m) = &a.mouse {
                    m.calibration(false, None);
                }
                a.notice = "Calibration cancelled. Your saved calibration is unchanged.".into();
            }
            a.overview = true;
            a.show_report = false;
        }
    });
    if full {
        toggle_fullscreen(hwnd);
    }
    ShowWindow(
        hwnd,
        if IsIconic(hwnd) != 0 {
            SW_RESTORE
        } else {
            SW_SHOW
        },
    );
    SetForegroundWindow(hwnd);
    layout(hwnd);
    refresh(hwnd);
    SetFocus(GetDlgItem(hwnd, HOME as i32));
    InvalidateRect(hwnd, null(), 0);
}
pub unsafe fn hide(hwnd: HWND) {
    let available = APP.with(|a| {
        a.borrow()
            .as_ref()
            .is_some_and(|a| a.tray.as_ref().is_some_and(|t| t.available))
    });
    if available {
        home(hwnd);
        ShowWindow(hwnd, SW_HIDE);
    } else {
        APP.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                a.notice =
                    "The tray icon is unavailable. Minimize the panel, or use Quit to exit.".into();
            }
        });
    }
}
pub unsafe fn command(hwnd: HWND, id: u32) {
    match id {
        1 => {
            // IsDialogMessage sends IDOK for Enter on an owner-drawn button.
            let focused = GetDlgCtrlID(GetFocus()) as u32;
            if focused == REARM_EDIT || focused == TRACKPAD_EDIT {
                command(hwnd, REARM_APPLY);
            } else if ALL.contains(&focused) {
                command(hwnd, focused);
            }
        }
        REARM_APPLY => {
            let read_delay = |id, min| {
                let mut text = [0u16; 32];
                let len = GetWindowTextW(GetDlgItem(hwnd, id), text.as_mut_ptr(), 32);
                String::from_utf16_lossy(&text[..len as usize])
                    .parse::<u32>()
                    .ok()
                    .filter(|ms| (min..=crate::preferences::MAX_REARM_MS).contains(ms))
            };
            let mouse = read_delay(REARM_EDIT as i32, crate::preferences::MIN_REARM_MS);
            let trackpad = read_delay(TRACKPAD_EDIT as i32, 0);
            APP.with(|a|if let Some(a)=a.borrow_mut().as_mut(){
                if let (Some(mouse),Some(trackpad))=(mouse,trackpad) {
                    if let Some(m)=&a.mouse {m.set_rearm_delay(mouse);m.set_trackpad_rearm_delay(trackpad);}
                    a.notice=format!("Jump timing applied: mouse {mouse} ms, trackpad {trackpad} ms. Trackpad still requires a lift.");
                } else {a.notice="Enter 50–2000 ms for mouse and 0–2000 ms for trackpad, then select Apply.".into();}
            });
        }
        HOME => home(hwnd),
        FEEDBACK => APP.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                a.preferences.learning_feedback = !a.preferences.learning_feedback;
                if !a.preferences.learning_feedback {
                    if let Some(overlay) = &mut a.learning_overlay {
                        overlay.hide();
                    }
                }
                a.notice = match a.preferences.save() {
                    Ok(()) => "Learning visuals changed. Learning itself is unchanged.".into(),
                    Err(error) => format!("Could not save visual preference: {error}"),
                };
            }
        }),
        FEEDBACK_DEMO => APP.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                if !a.preferences.learning_feedback {
                    return;
                }
                if let Some(overlay) = &mut a.learning_overlay {
                    let rect = display(hwnd).rect;
                    let center = [
                        (rect[0] + rect[2]) as f64 / 2.,
                        (rect[1] + rect[3]) as f64 / 2.,
                    ];
                    let before = [center[0] - 65., center[1]];
                    overlay.show(
                        crate::learning::Feedback {
                            landed: before,
                            before,
                            after: [before[0] + 12., before[1] - 4.],
                            selection: Some(center),
                            rect,
                            accepted: true,
                            updated: true,
                            reason: "Preview animation",
                            demo: true,
                        },
                        GetDpiForWindow(hwnd) as f64 / 96.,
                    );
                }
            }
        }),
        HIDE => hide(hwnd),
        QUIT => {
            DestroyWindow(hwnd);
        }
        POWER | DOT | SCROLL | LEARN | RESET => {
            if id==RESET && MessageBoxW(hwnd,wide("Clear this session’s learned corrections? Saved journals and calibration are kept.").as_ptr(),wide("Reset learning").as_ptr(),MB_OKCANCEL|MB_ICONQUESTION)!=IDOK {return;}
            APP.with(|a| {
                if let Some(m) = a.borrow().as_ref().and_then(|a| a.mouse.as_ref()) {
                    m.command(match id {
                        POWER => 1,
                        DOT => 4,
                        SCROLL => 6,
                        LEARN => 2,
                        _ => 3,
                    });
                }
            });
        }
        HISTORY => APP.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                a.notice = match a.mouse.as_ref().map(|m| m.open_learning_map()) {
                    Some(Ok(())) => "Opened a snapshot of your learning map in the browser.".into(),
                    Some(Err(e)) => e,
                    None => "Controller unavailable.".into(),
                };
            }
        }),
        PREVIEW | RESULTS => {
            APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    a.overview = false;
                    a.show_report = id == RESULTS;
                }
            });
            layout(hwnd);
            SetFocus(hwnd);
        }
        REFIT => {
            APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    let result = a.report.as_ref().ok_or("No saved calibration.".to_string())
                        .and_then(Report::refit)
                        .and_then(|r| { r.save()?; Ok(r) });
                    match result {
                        Ok(report) => {
                            a.correction = report.metrics.recommend;
                            if let Some(mouse) = &a.mouse { mouse.calibration(false, Some(&report)); }
                            a.report = Some(report);
                            a.show_report = true;
                            a.notice = "Refitted the saved fitting points. Original recording kept; check results are a replay.".into();
                        }
                        Err(error) => a.notice = format!("Could not refit: {error}"),
                    }
                }
            });
            layout(hwnd);
        }
        CALIBRATE => {
            APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    a.overview = false;
                }
            });
            SendMessageW(hwnd, WM_KEYDOWN, 0x43, 0);
            layout(hwnd);
            SetFocus(hwnd);
        }
        CAPTURE => {
            SendMessageW(hwnd, WM_KEYDOWN, 0x20, 0);
            SetFocus(hwnd);
        }
        CANCEL => home(hwnd),
        FULLSCREEN => {
            toggle_fullscreen(hwnd);
            layout(hwnd);
        }
        DISPLAY => next_monitor(hwnd),
        CORRECTION | TRAIL | TARGETS => {
            SendMessageW(
                hwnd,
                WM_KEYDOWN,
                match id {
                    CORRECTION => 0x41,
                    TRAIL => 0x54,
                    _ => 0x47,
                },
                0,
            );
        }
        _ => {}
    }
    refresh(hwnd);
    InvalidateRect(hwnd, null(), 0);
}
