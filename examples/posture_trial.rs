//! Guided known-target experiment. This process has no mouse controller.
//! Position-dependent baseline and pose models are scored before training;
//! held-out points and the return visit never train the experimental models.
//! Round transitions require Enter separately from Space captures, preventing
//! repeated capture gestures from silently skipping a posture change. Q exits
//! without conflicting with the computer-use tool's global Escape stop key.
#![allow(dead_code)]
#![windows_subsystem = "windows"]
#[path = "../src/calibration.rs"]
mod calibration;
#[path = "../src/capture.rs"]
mod capture;
#[path = "../src/corner.rs"]
mod corner;
#[path = "../src/learning.rs"]
mod learning;
#[path = "../src/learning_log.rs"]
mod learning_log;
#[path = "../src/learning_store.rs"]
mod learning_store;
#[path = "../src/pose.rs"]
mod pose;
#[path = "../src/posture_model.rs"]
mod posture_model;
#[path = "../src/tobii.rs"]
mod tobii;
#[path = "../src/tobii_capture.rs"]
mod tobii_capture;
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    mem::zeroed,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};
const TARGETS: [[f64; 2]; 13] = [
    [0.15, 0.15],
    [0.5, 0.5],
    [0.85, 0.85],
    [0.85, 0.15],
    [0.15, 0.85],
    [0.5, 0.15],
    [0.85, 0.5],
    [0.5, 0.85],
    [0.15, 0.5],
    [0.3, 0.3],
    [0.7, 0.7],
    [0.3, 0.7],
    [0.7, 0.3],
];
struct Trial {
    worker: tobii::Worker,
    report: Option<calibration::Report>,
    rect: [i32; 4],
    step: usize,
    capture: Option<Instant>,
    last: i64,
    samples: Vec<pose::Evidence>,
    rows: Vec<Value>,
    models: posture_model::Models,
    baseline: learning::Learner,
    notice: String,
    path: std::path::PathBuf,
    complete: bool,
    confirmed_blocks: Vec<usize>,
}
fn posture_confirmed(step: usize, confirmed_blocks: &[usize]) -> bool {
    step < 39 && confirmed_blocks.contains(&(step / 13))
}
thread_local! {static TRIAL:RefCell<Option<Trial>>=const{RefCell::new(None)};}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}
impl Trial {
    fn block(&self) -> usize {
        self.step / 13
    }
    fn target(&self) -> [f64; 2] {
        TARGETS[self.step % 13]
    }
    fn tick(&mut self) {
        let Some(start) = self.capture else {
            return;
        };
        let elapsed = start.elapsed();
        {
            let state = self.worker.state.lock().unwrap();
            for sample in &state.samples {
                if sample.timestamp_us <= self.last {
                    continue;
                }
                self.last = sample.timestamp_us;
                if sample.valid
                    && sample.received >= start + Duration::from_millis(400)
                    && sample.received <= start + Duration::from_millis(1400)
                {
                    let evidence = state.evidence(*sample);
                    if evidence.head.is_some() {
                        self.samples.push(evidence);
                    }
                }
            }
        }
        if elapsed < Duration::from_millis(1450) {
            return;
        }
        self.capture = None;
        if self.samples.len() < 20 {
            self.notice = format!(
                "Only {} synchronized samples. Face the tracker, then press Space to retry.",
                self.samples.len()
            );
            return;
        }
        let raw: [f64; 2] =
            std::array::from_fn(|i| median(self.samples.iter().map(|s| s.raw_gaze[i]).collect()));
        let raw = self
            .report
            .as_ref()
            .filter(|r| r.metrics.recommend)
            .map_or(raw, |r| r.model.apply(raw));
        let size = [
            (self.rect[2] - self.rect[0]) as f64,
            (self.rect[3] - self.rect[1]) as f64,
        ];
        let scatter = median(
            self.samples
                .iter()
                .map(|s| {
                    ((s.raw_gaze[0] - median(self.samples.iter().map(|s| s.raw_gaze[0]).collect()))
                        * size[0])
                        .hypot(
                            (s.raw_gaze[1]
                                - median(self.samples.iter().map(|s| s.raw_gaze[1]).collect()))
                                * size[1],
                        )
                })
                .collect(),
        );
        if scatter > 70.0 {
            self.notice =
                "Gaze moved too much. Keep looking at the dot and press Space to retry.".into();
            return;
        }
        let p: [f64; 6] = std::array::from_fn(|i| {
            median(
                self.samples
                    .iter()
                    .map(|s| {
                        let h = s.head.unwrap();
                        if i < 3 {
                            h.position[i]
                        } else {
                            h.rotation[i - 3]
                        }
                    })
                    .collect(),
            )
        });
        let target = self.target();
        let datum = posture_model::Datum {
            gaze: raw,
            pose: p,
            error: std::array::from_fn(|i| (target[i] - raw[i]) * size[i]),
        };
        let base: [f64; 2] = std::array::from_fn(|i| self.rect[i] as f64 + raw[i] * size[i]);
        let current = self.baseline.offset_at(base, self.rect);
        let predictions = self.models.predict(&datum);
        let training = self.block() < 2 && self.step % 13 < 9;
        let row = json!({"kind":"capture","block":self.block(),"step":self.step,"training":training,"target":target,"datum":datum,
   "predictions":{"base":[0.0,0.0],"current_spatial":current,"shared_affine":predictions[0],"continuous_pose":predictions[1],"pose_bank":predictions[2]},
   "profiles_before":self.models.profiles(),"samples":self.samples,"scatter_px":scatter});
        self.rows.push(row);
        if training {
            self.baseline.observe(
                (self.step * 2000) as u32,
                datum.gaze,
                datum.error,
                self.rect,
            );
            self.models.train(datum);
        }
        self.step += 1;
        self.complete = self.step >= 39;
        self.notice = if self.complete {
            "Finished. Results saved. Press Q to close the test.".into()
        } else {
            "Ready. Settle into the indicated posture, look at the dot, then press Space.".into()
        };
        if let Err(e) = self.save() {
            self.notice = format!("SAVE FAILED: {e}");
        }
    }
    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.path.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut summary = serde_json::Map::new();
        for block in 0..3 {
            let held: Vec<_> = self
                .rows
                .iter()
                .filter(|r| r["block"] == block && r["training"] == false)
                .collect();
            let mut metrics = serde_json::Map::new();
            for name in [
                "base",
                "current_spatial",
                "shared_affine",
                "continuous_pose",
                "pose_bank",
            ] {
                let errors: Vec<f64> = held
                    .iter()
                    .map(|r| {
                        let e = &r["datum"]["error"];
                        let p = &r["predictions"][name];
                        (e[0].as_f64().unwrap() - p[0].as_f64().unwrap())
                            .hypot(e[1].as_f64().unwrap() - p[1].as_f64().unwrap())
                    })
                    .collect();
                if !errors.is_empty() {
                    let mut sorted = errors.clone();
                    sorted.sort_by(f64::total_cmp);
                    metrics.insert(name.into(),json!({"n":errors.len(),"mean_px":errors.iter().sum::<f64>()/errors.len() as f64,"median_px":median(errors),"p90_px":sorted[((sorted.len() as f64*0.9).ceil() as usize-1).min(sorted.len()-1)]}));
                }
            }
            summary.insert(block.to_string(), Value::Object(metrics));
        }
        let report = json!({"schema":2,"kind":"guided_posture_trial","complete":self.complete,"validation_status":"pending_review","confirmed_blocks":self.confirmed_blocks,"rect":self.rect,"base_calibration":self.report,
    "summary":summary,"captures":self.rows,"notes":"Prototype comparison; training clicks are simulated with known fixation targets. Held-out points and return visit do not train. No model controls cursor."});
        std::fs::write(
            &self.path,
            serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}
unsafe fn text(dc: HDC, x: i32, y: i32, s: &str) {
    let v = wide(s);
    TextOutW(dc, x, y, v.as_ptr(), (v.len() - 1) as i32);
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_KILLFOCUS => {
            TRIAL.with(|t| {if let Some(t)=t.borrow_mut().as_mut(){if t.capture.take().is_some(){t.samples.clear();t.notice="Capture cancelled while window was inactive. Look at the dot and press Space.".into();}}});
            0
        }
        WM_KEYDOWN if w == 0x51 => {
            DestroyWindow(hwnd);
            0
        }
        WM_KEYDOWN if w == 13 && l & (1 << 30) == 0 => {
            TRIAL.with(|t| {
                if let Some(t) = t.borrow_mut().as_mut() {
                    if !t.complete && !posture_confirmed(t.step, &t.confirmed_blocks) {
                        t.confirmed_blocks.push(t.block());
                        t.notice = "Posture confirmed. Look at the dot, then press Space.".into();
                        if let Err(e) = t.save() {
                            t.notice = format!("SAVE FAILED: {e}");
                        }
                    }
                }
            });
            0
        }
        WM_KEYDOWN if w == 32 && l & (1 << 30) == 0 => {
            TRIAL.with(|t| {
                if let Some(t) = t.borrow_mut().as_mut() {
                    if posture_confirmed(t.step, &t.confirmed_blocks) && t.capture.is_none() {
                        t.capture = Some(Instant::now());
                        t.samples.clear();
                        t.notice = "Capturing: keep looking at the dot…".into();
                    }
                }
            });
            0
        }
        WM_TIMER => {
            TRIAL.with(|t| {
                if let Some(t) = t.borrow_mut().as_mut() {
                    t.tick();
                }
            });
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            let bg = CreateSolidBrush(0x00201810);
            FillRect(dc, &r, bg);
            DeleteObject(bg);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, 0x00eee8e0);
            let font = CreateFontW(
                32,
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
            let old = SelectObject(dc, font);
            TRIAL.with(|t|{if let Some(t)=t.borrow().as_ref(){
   let title=if t.complete{"Posture test complete"}else{["Round 1 / 3: sit upright as you normally do","Round 2 / 3: lean back comfortably; keep eyes visible","Round 3 / 3: return to your original upright position"][t.block()]};
   text(dc,40,35,title);text(dc,40,85,&t.notice);
   text(dc,40,r.bottom-100,&format!("{} / 39 captures. Space starts a 1.4 s capture. Q saves progress and exits.",t.step));
   text(dc,40,r.bottom-55,&t.path.display().to_string());
   if !t.complete && !posture_confirmed(t.step, &t.confirmed_blocks) {
    let headline = ["SIT UPRIGHT", "CHANGE POSTURE: LEAN BACK", "RETURN TO YOUR ORIGINAL UPRIGHT POSTURE"][t.block()];
    let large = CreateFontW(64,0,0,0,FW_BOLD as i32,0,0,0,DEFAULT_CHARSET as u32,0,0,CLEARTYPE_QUALITY as u32,0,wide("Segoe UI").as_ptr());
    let previous = SelectObject(dc, large);
    SetTextAlign(dc, TA_CENTER);
    SetTextColor(dc, 0x0066ccff);
    text(dc,r.right/2,r.bottom/2-100,headline);
    SelectObject(dc,previous);DeleteObject(large);
    SetTextColor(dc, 0x00eee8e0);
    text(dc,r.right/2,r.bottom/2,"Settle into this posture and keep your eyes visible to the tracker.");
    text(dc,r.right/2,r.bottom/2+60,"Press ENTER when ready. Space cannot skip this screen.");
    text(dc,r.right/2,r.bottom/2+120,"Stay in this posture for all 13 dots in this round.");
    SetTextAlign(dc, TA_LEFT);
   }
   if posture_confirmed(t.step, &t.confirmed_blocks){let p=t.target();let x=(p[0]*r.right as f64) as i32;let y=(p[1]*r.bottom as f64) as i32;
    let brush=CreateSolidBrush(if t.capture.is_some(){0x00aade55}else{0x00ffffff});let oldbrush=SelectObject(dc,brush);Ellipse(dc,x-12,y-12,x+12,y+12);SelectObject(dc,oldbrush);DeleteObject(brush);}
  }});
            SelectObject(dc, old);
            DeleteObject(font);
            EndPaint(hwnd, &ps);
            0
        }
        WM_DESTROY => {
            TRIAL.with(|t| {
                if let Some(t) = t.borrow().as_ref() {
                    let _ = t.save();
                }
            });
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
fn main() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let report = calibration::Report::load_latest();
        let rect = report.as_ref().map(|r| r.display.rect).unwrap_or([
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        ]);
        let path = std::env::current_dir()
            .unwrap()
            .join("recordings")
            .join(format!(
                "posture-trial-{}.json",
                learning_log::timestamp_ms()
            ));
        TRIAL.with(|t| {
            *t.borrow_mut() = Some(Trial {
                worker: tobii::Worker::start(),
                report,
                rect,
                step: 0,
                capture: None,
                last: 0,
                samples: Vec::new(),
                rows: Vec::new(),
                models: posture_model::Models::default(),
                baseline: learning::Learner::default(),
                notice: "Follow the posture instructions in the center of the screen.".into(),
                path,
                complete: false,
                confirmed_blocks: Vec::new(),
            })
        });
        let name = wide("PostureTrial");
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            name.as_ptr(),
            wide("Gaze posture experiment").as_ptr(),
            WS_POPUP,
            rect[0],
            rect[1],
            rect[2] - rect[0],
            rect[3] - rect[1],
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if !hwnd.is_null() {
            SetTimer(hwnd, 1, 30, None);
            ShowWindow(hwnd, SW_SHOW);
            let mut msg = zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        TRIAL.with(|t| {
            t.borrow_mut().take();
        });
    }
}

#[cfg(test)]
mod trial_tests {
    use super::posture_confirmed;

    #[test]
    fn captures_require_separate_confirmation_for_each_round() {
        assert!(!posture_confirmed(0, &[]));
        assert!(posture_confirmed(0, &[0]));
        assert!(posture_confirmed(12, &[0]));
        assert!(!posture_confirmed(13, &[0]));
        assert!(posture_confirmed(13, &[0, 1]));
        assert!(!posture_confirmed(26, &[0, 1]));
        assert!(posture_confirmed(38, &[0, 1, 2]));
        assert!(!posture_confirmed(39, &[0, 1, 2]));
    }
}
