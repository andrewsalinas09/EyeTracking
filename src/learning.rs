//! Persistent implicit calibration. Clicks are noisy labels, not ground truth.
//! Store click minus the BASE gaze map, not minus the already corrected landing;
//! otherwise applying each residual repeatedly would create a feedback drift.
//! Spatial errors cannot be represented by one global offset: learn a local
//! residual field and interpolate it continuously at the base gaze position.
//! Rejections may be detected during movement, but visual feedback is emitted
//! only after a matching left-button release, never by movement or timeout alone.
//! Corrections can cross window boundaries (including the taskbar). A click may
//! also replace its own window; screen distance and drag checks validate intent.
use crate::learning_store::{SavedLabel, SavedState, Store};
use serde::Serialize;
use serde_json::json;
use std::collections::VecDeque;

type XY = [f64; 2];
const MAX_CLICK_MS: u32 = 3000;
const HISTORY_MS: u32 = 300_000;
const MAX_CORRECTION: f64 = 300.0;
const MAX_OFFSET: f64 = 300.0;
const CONSENSUS_RADIUS: f64 = 40.0;
const MAX_STEP: f64 = 6.0;
const COLS: usize = 65;
const ROWS: usize = 37;
const MAX_LABELS: usize = 1024;

#[derive(Clone, Copy, Default, Serialize)]
struct Node {
    offset: XY,
    trained: bool,
}

struct Attempt {
    interaction: u64,
    evidence: Option<crate::pose::Evidence>,
    time: u32,
    base: XY,
    landing: XY,
    last: XY,
    path: f64,
    surface: usize,
    last_surface: usize,
    last_flags: u16,
    last_time: u32,
    rect: [i32; 4],
    down: Option<(u32, XY)>,
    drag_path: f64,
    rejection: Option<&'static str>,
}
struct Label {
    time: u32,
    timestamp_ms: u64,
    position: XY,
    offset: XY,
}
/// Read-only evidence for the optional animation. `after` is a prediction at
/// the original gaze, never a command to move the real pointer to the click.
#[derive(Clone, Debug)]
pub struct Feedback {
    pub landed: XY,
    pub before: XY,
    pub after: XY,
    pub selection: Option<XY>,
    pub rect: [i32; 4],
    pub accepted: bool,
    pub updated: bool,
    pub reason: &'static str,
    pub demo: bool,
}
pub struct Learner {
    pub enabled: bool,
    field: [Node; COLS * ROWS],
    pub accepted: u64,
    pub updates: u64,
    pub rejected: u64,
    pub status: &'static str,
    pending: Option<Attempt>,
    labels: VecDeque<Label>,
    recorder: Option<crate::learning_log::Recorder>,
    recording_error: String,
    epoch: u64,
    feedback: Option<Feedback>,
    store: Option<Store>,
    context_key: Option<String>,
    storage_error: String,
    interaction: Option<u64>,
    corner: Option<crate::corner::Model>,
    pub corner_enabled: bool,
}
impl Default for Learner {
    fn default() -> Self {
        Self {
            enabled: true,
            field: [Node::default(); COLS * ROWS],
            accepted: 0,
            updates: 0,
            rejected: 0,
            status: "Waiting for jump + click",
            pending: None,
            labels: VecDeque::new(),
            recorder: None,
            recording_error: String::new(),
            epoch: 0,
            feedback: None,
            store: None,
            context_key: None,
            storage_error: String::new(),
            interaction: None,
            corner: None,
            corner_enabled: false,
        }
    }
}
fn distance(a: XY, b: XY) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn bounded(v: XY, max: f64) -> XY {
    let length = v[0].hypot(v[1]);
    if length > max {
        v.map(|x| x * max / length)
    } else {
        v
    }
}
fn interior(p: XY, r: [i32; 4]) -> bool {
    p.iter().all(|x| x.is_finite())
        && p[0] >= r[0] as f64 + 4.0
        && p[0] < r[2] as f64 - 4.0
        && p[1] >= r[1] as f64 + 4.0
        && p[1] < r[3] as f64 - 4.0
}
impl Learner {
    pub fn take_feedback(&mut self) -> Option<Feedback> {
        self.feedback.take()
    }
    fn publish_feedback(
        &mut self,
        p: &Attempt,
        before: XY,
        accepted: bool,
        updated: bool,
        reason: &'static str,
    ) {
        let offset = self.offset_at(p.base, p.rect);
        self.feedback = Some(Feedback {
            landed: p.landing,
            before,
            after: if updated {
                [p.base[0] + offset[0], p.base[1] + offset[1]]
            } else {
                before
            },
            selection: p.down.map(|(_, target)| target),
            rect: p.rect,
            accepted,
            updated,
            reason,
            demo: false,
        });
    }
    pub fn interaction(&self) -> Option<u64> {
        self.interaction
    }
    pub fn record_jump(&mut self, source: &str, success: bool) {
        self.interaction = Some(crate::capture::interaction_id());
        if let Some(log) = &self.recorder {
            log.record(json!({"kind":"jump", "source":source, "success":success}));
        }
    }
    pub fn start_recording(
        &mut self,
        display: &crate::calibration::Display,
        model: Option<&crate::calibration::Model>,
    ) {
        let result = std::env::current_dir()
            .map_err(|e| e.to_string())
            .and_then(|p| crate::learning_log::Recorder::start(&p.join("recordings")));
        match result {
            Ok(recorder) => {
                self.recorder = Some(recorder);
            }
            Err(e) => self.recording_error = format!("Learning save unavailable: {e}"),
        }
        match Store::open(std::path::Path::new("recordings/learning.sqlite3")) {
            Ok(store) => self.store = Some(store),
            Err(e) => self.storage_error = e,
        }
        self.change_context(display, model);
    }
    pub fn storage_status(&self) -> String {
        if !self.storage_error.is_empty() {
            return format!("Learning database error: {}", self.storage_error);
        }
        let storage = self
            .store
            .as_ref()
            .map_or_else(|| "Learning is not saved".into(), Store::status);
        if storage.contains("error") || self.store.is_none() {
            storage
        } else {
            crate::capture::status().unwrap_or(storage)
        }
    }
    pub fn change_context(
        &mut self,
        display: &crate::calibration::Display,
        model: Option<&crate::calibration::Model>,
    ) {
        // Only the actual mapping matters, not a report's date or validation score.
        let key = json!({"display":display,"mapping":model.map(|m|json!({"coefficients":m.coefficients,"local":m.local}))}).to_string();
        if self.context_key.as_ref() == Some(&key) {
            return;
        }
        self.reset();
        self.context_key = Some(key.clone());
        self.corner = crate::corner::Model::load(&key);
        if let Some(store) = &self.store {
            match store.load(key) {
                Ok(Some(state)) => {
                    if let Err(e) = self.restore(state, display.rect) {
                        self.storage_error = e;
                    }
                }
                Ok(None) => {}
                Err(e) => self.storage_error = e,
            }
        }
        self.record_context(display, model);
    }
    fn restore(&mut self, state: SavedState, rect: [i32; 4]) -> Result<(), String> {
        if state.version != 1
            || state.cols != COLS
            || state.rows != ROWS
            || state.offsets.len() != COLS * ROWS
            || state.trained.len() != COLS * ROWS
            || state
                .offsets
                .iter()
                .any(|p| !p.iter().all(|x| x.is_finite()) || p[0].hypot(p[1]) > MAX_OFFSET + 1e-6)
            || state.labels.iter().any(|s| {
                !s.position
                    .iter()
                    .all(|x| x.is_finite() && (0.0..=1.0).contains(x))
                    || !s.offset.iter().all(|x| x.is_finite())
            })
        {
            return Err("Saved map is incompatible or invalid; it was not applied".into());
        }
        let field = std::array::from_fn(|i| Node {
            offset: state.offsets[i],
            trained: state.trained[i],
        });
        if !field_is_stable(&field, rect) {
            return Err("Saved map failed smoothness validation".into());
        }
        self.field = field;
        self.accepted = state.accepted;
        self.updates = state.updates;
        let wall = crate::learning_log::timestamp_ms();
        let tick = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount() };
        self.labels = state
            .labels
            .into_iter()
            .filter_map(|s| {
                let age = wall.checked_sub(s.timestamp_ms)?;
                (age <= HISTORY_MS as u64).then_some(Label {
                    time: tick.wrapping_sub(age as u32),
                    timestamp_ms: s.timestamp_ms,
                    position: s.position,
                    offset: s.offset,
                })
            })
            .take(MAX_LABELS)
            .collect();
        self.status = "Restored saved learning";
        Ok(())
    }
    pub fn reset_persistent(&mut self) -> Result<(), String> {
        self.store
            .as_ref()
            .ok_or_else(|| self.storage_status())?
            .reset()?;
        self.reset();
        self.storage_error.clear();
        self.status = "Saved learning cleared";
        Ok(())
    }
    fn persist(&self) {
        if let (Some(store), Some(key), Some(last)) =
            (&self.store, &self.context_key, self.labels.back())
        {
            let saved = |s: &Label| SavedLabel {
                timestamp_ms: s.timestamp_ms,
                position: s.position,
                offset: s.offset,
            };
            store.save(
                key.clone(),
                SavedState {
                    version: 1,
                    cols: COLS,
                    rows: ROWS,
                    offsets: self.field.iter().map(|n| n.offset).collect(),
                    trained: self.field.iter().map(|n| n.trained).collect(),
                    labels: self.labels.iter().map(saved).collect(),
                    accepted: self.accepted,
                    updates: self.updates,
                },
                saved(last),
            );
        }
    }
    pub fn corner_available(&self) -> bool {
        self.corner.is_some()
    }
    pub fn toggle_corner(&mut self) {
        self.cancel("Skipped: corner trial changed");
        self.corner_enabled = !self.corner_enabled && self.corner.is_some();
        crate::capture::record(
            "corner_trial",
            None,
            json!({"enabled":self.corner_enabled,"model":self.corner,"region":"upper_left","max_extra_pixels":150}),
        );
    }
    pub fn record_context(
        &self,
        display: &crate::calibration::Display,
        model: Option<&crate::calibration::Model>,
    ) {
        let data = json!({"kind":"context", "schema":1, "epoch":self.epoch,
                "display":display, "base_model":model, "grid":[COLS,ROWS], "field":self.field.as_slice(),
                "coordinates":"physical desktop pixels; base gaze is after fixed calibration",
                "corner_model":self.corner,"corner_enabled":self.corner_enabled,
                "learner":"spatial-v2.1", "rules":{"max_click_ms":MAX_CLICK_MS,"max_correction_px":MAX_CORRECTION,"max_path_px":600,"min_click_ms":80,"max_hold_ms":500,"max_drag_px":4,
                    "max_step_px":MAX_STEP,"max_offset_px":MAX_OFFSET,"consensus_radius_px":CONSENSUS_RADIUS,"history_ms":HISTORY_MS,"max_labels":MAX_LABELS,"enabled":self.enabled}});
        crate::capture::record("context", None, data.clone());
        if let Some(log) = &self.recorder {
            log.record(data);
        }
    }
    pub fn open_map(&self) -> Result<(), String> {
        self.recorder
            .as_ref()
            .ok_or_else(|| self.recording_status())?
            .open()
    }
    pub fn recording_status(&self) -> String {
        self.recorder
            .as_ref()
            .map_or_else(|| self.recording_error.clone(), |r| r.status())
    }
    fn record_attempt(&self, p: &Attempt, accepted: bool, updated: bool, reason: &str) {
        let data = json!({"kind":"attempt", "epoch":self.epoch, "rect":p.rect,
                "base":p.base, "landing":p.landing, "target":p.down.map(|(_,target)|target),
                "last":p.last, "path_px":p.path, "accepted":accepted, "updated":updated,
                "status":reason, "elapsed_ms":p.last_time.wrapping_sub(p.time),
                "last_input_flags":p.last_flags, "landing_surface":p.surface,"last_surface":p.last_surface,
                "jump_tick_ms":p.time,"end_tick_ms":p.last_time,"button_down_tick_ms":p.down.map(|(t,_)|t),
                "click_delay_ms":p.down.map(|(t,_)|t.wrapping_sub(p.time)),"drag_path_px":p.drag_path,
                "button_up_observed":p.last_flags&2!=0,"interaction":p.interaction,"corner_enabled":self.corner_enabled,
                "grid":[COLS,ROWS], "field":updated.then_some(self.field.as_slice()), "evidence":p.evidence});
        crate::capture::record("attempt", Some(p.interaction), data.clone());
        if let Some(log) = &self.recorder {
            log.record(data);
        }
    }
    /// Bilinear interpolation gives a continuous correction, without snapping
    /// between cells. Untrained nodes contribute zero rather than extrapolation.
    pub fn offset_at(&self, base: XY, rect: [i32; 4]) -> XY {
        let p = normalized(base, rect);
        let x = p[0].clamp(0.0, 1.0) * (COLS - 1) as f64;
        let y = p[1].clamp(0.0, 1.0) * (ROWS - 1) as f64;
        let ix = (x.floor() as usize).min(COLS - 2);
        let iy = (y.floor() as usize).min(ROWS - 2);
        let fx = x - ix as f64;
        let fy = y - iy as f64;
        let corners = [
            (ix + iy * COLS, (1.0 - fx) * (1.0 - fy)),
            (ix + 1 + iy * COLS, fx * (1.0 - fy)),
            (ix + (iy + 1) * COLS, (1.0 - fx) * fy),
            (ix + 1 + (iy + 1) * COLS, fx * fy),
        ];
        let offset = std::array::from_fn(|axis| {
            corners
                .iter()
                .map(|(i, w)| self.field[*i].offset[axis] * w)
                .sum()
        });
        if self.corner_enabled {
            self.corner
                .as_ref()
                .map_or(offset, |m| m.offset(base, offset, rect))
        } else {
            offset
        }
    }
    pub fn coverage(&self) -> (usize, usize) {
        (
            self.field.iter().filter(|n| n.trained).count(),
            self.field.len(),
        )
    }
    pub fn reset(&mut self) {
        self.cancel("Skipped: calibration reset");
        let enabled = self.enabled;
        let recorder = self.recorder.take();
        let error = std::mem::take(&mut self.recording_error);
        let epoch = self.epoch + 1;
        let store = self.store.take();
        let context_key = self.context_key.take();
        let storage_error = std::mem::take(&mut self.storage_error);
        *self = Self::default();
        self.store = store;
        self.context_key = context_key;
        self.storage_error = storage_error;
        self.enabled = enabled;
        self.recorder = recorder;
        self.recording_error = error;
        self.epoch = epoch;
        crate::capture::record(
            "learning_reset",
            None,
            json!({"epoch":epoch,"research_archive_retained":true}),
        );
        if let Some(log) = &self.recorder {
            log.record(json!({"kind":"reset", "epoch":epoch, "grid":[COLS,ROWS], "field":self.field.as_slice()}));
        }
    }
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.cancel("Learning frozen");
    }
    pub fn cancel(&mut self, reason: &'static str) {
        if let Some(p) = self.pending.take() {
            let reason = p.rejection.unwrap_or(reason);
            self.rejected += 1;
            self.status = reason;
            self.record_attempt(&p, false, false, reason);
        }
    }
    pub fn expire(&mut self, now: u32) {
        if self.pending.as_ref().is_some_and(|p| match p.down {
            Some(_) => now.wrapping_sub(p.time) > MAX_CLICK_MS + 500,
            None => now.wrapping_sub(p.time) > MAX_CLICK_MS,
        }) {
            if let Some(p) = self.pending.as_mut() {
                p.last_time = now;
            }
            self.cancel("Skipped: delayed click");
        }
    }
    pub fn begin(&mut self, now: u32, base: XY, landing: XY, surface: usize, rect: [i32; 4]) {
        self.cancel("Replaced by next jump");
        if !base.iter().chain(landing.iter()).all(|v| v.is_finite()) {
            return;
        }
        let interaction = *self
            .interaction
            .get_or_insert_with(crate::capture::interaction_id);
        let rejection = if !self.enabled {
            Some("Skipped: learning frozen")
        } else if surface == 0 {
            Some("Skipped: no target surface")
        } else if !interior(base, rect) || !interior(landing, rect) {
            Some("Skipped: edge landing")
        } else {
            None
        };
        crate::capture::record(
            "attempt_started",
            Some(interaction),
            json!({"base":base,"landing":landing,"rect":rect,"surface":surface,"jump_tick_ms":now,"initial_rejection":rejection,"epoch":self.epoch}),
        );
        self.pending = Some(Attempt {
            interaction,
            evidence: None,
            time: now,
            base,
            landing,
            last: landing,
            path: 0.0,
            surface,
            last_surface: surface,
            last_flags: 0,
            last_time: now,
            rect,
            down: None,
            drag_path: 0.0,
            rejection,
        });
        self.status = "Watching correction + click";
    }
    pub fn attach_evidence(&mut self, evidence: crate::pose::Evidence) {
        if let Some(p) = &mut self.pending {
            p.evidence = Some(evidence);
            crate::capture::record("jump_evidence", Some(p.interaction), json!(evidence));
        }
    }
    /// Flags are Win32 raw mouse left-down/up bits 1/2. All other buttons and
    /// scrolling invalidate the attempt. Commit only on release to reject drags.
    pub fn event(&mut self, now: u32, cursor: XY, flags: u16, surface: usize) {
        self.expire(now);
        let Some(p) = self.pending.as_mut() else {
            return;
        };
        p.last_surface = surface;
        p.last_flags = flags;
        p.last_time = now;
        let invalid = if !self.enabled {
            Some("Skipped: learning paused")
        } else if flags & (0x0400 | 0x0800) != 0 {
            Some("Skipped: scrolling")
        } else if flags & !3 != 0 {
            Some("Skipped: other mouse button")
        } else if !cursor
            .iter()
            .enumerate()
            .all(|(i, x)| x.is_finite() && *x >= p.rect[i] as f64 && *x < p.rect[i + 2] as f64)
        {
            Some("Skipped: outside calibrated display")
        } else {
            None
        };
        if let Some(reason) = invalid {
            self.cancel(reason);
            return;
        }
        let travel = distance(cursor, p.last);
        p.path += travel;
        p.last = cursor;
        if p.down.is_some() {
            p.drag_path += travel;
        }
        if p.path > 600.0 || distance(cursor, p.landing) > MAX_CORRECTION {
            p.rejection.get_or_insert("Skipped: long correction");
        }
        if p.drag_path > 4.0 {
            p.rejection.get_or_insert("Skipped: drag");
        }
        if flags & 1 != 0 {
            if p.down.is_some() || now.wrapping_sub(p.time) < 80 {
                p.rejection.get_or_insert("Skipped: ambiguous click");
            }
            p.down.get_or_insert((now, cursor));
        }
        if let Some(reason) = p.rejection {
            self.status = reason;
        }
        if flags & 2 != 0 {
            let Some((down_time, target)) = p.down else {
                self.cancel("Skipped: unmatched release");
                return;
            };
            if now.wrapping_sub(down_time) > 500 {
                p.rejection.get_or_insert("Skipped: held click");
            }
            let label = [target[0] - p.base[0], target[1] - p.base[1]];
            let position = normalized(p.base, p.rect);
            let rect = p.rect;
            let attempt = self.pending.take().unwrap();
            if let Some(reason) = attempt.rejection {
                self.rejected += 1;
                self.status = reason;
                self.record_attempt(&attempt, false, false, reason);
                self.publish_feedback(&attempt, attempt.landing, false, false, reason);
                return;
            }
            let previous = self.updates;
            let offset = self.offset_at(attempt.base, rect);
            let before = [attempt.base[0] + offset[0], attempt.base[1] + offset[1]];
            self.observe(now, position, label, rect);
            self.record_attempt(&attempt, true, self.updates != previous, self.status);
            self.publish_feedback(
                &attempt,
                before,
                true,
                self.updates != previous,
                self.status,
            );
        }
    }
    pub fn observe(&mut self, now: u32, position: XY, offset: XY, rect: [i32; 4]) {
        self.observe_inner(now, position, offset, rect);
        self.persist();
    }
    fn observe_inner(&mut self, now: u32, position: XY, offset: XY, rect: [i32; 4]) {
        self.labels
            .retain(|s| now.wrapping_sub(s.time) <= HISTORY_MS);
        self.labels.push_back(Label {
            time: now,
            timestamp_ms: crate::learning_log::timestamp_ms(),
            position,
            offset,
        });
        while self.labels.len() > MAX_LABELS {
            self.labels.pop_front();
        }
        self.accepted += 1;
        if self.labels.len() < 3 {
            self.status = "Gathering 3 local clicks";
            return;
        }
        let before = self.field;
        let mut proposed = before;
        let mut agreed = false;
        for (index, node) in proposed.iter_mut().enumerate() {
            let center = [
                (index % COLS) as f64 / (COLS - 1) as f64,
                (index / COLS) as f64 / (ROWS - 1) as f64,
            ];
            let influence = spatial_weight(position, center, offset, rect);
            if influence == 0.0 {
                continue;
            }
            // Independent recent neighborhoods prevent left/right corrections
            // from fighting each other in a single global outlier calculation.
            let nearby: Vec<_> = self
                .labels
                .iter()
                .rev()
                .filter(|s| spatial_weight(s.position, center, s.offset, rect) > 0.0)
                .take(9)
                .map(|s| {
                    (
                        s,
                        spatial_weight(s.position, center, s.offset, rect)
                            * 2_f64.powf(-(now.wrapping_sub(s.time) as f64) / 60_000.0),
                    )
                })
                .collect();
            if nearby.len() < 3 {
                continue;
            }
            let median = weighted_median(&nearby);
            let inliers: Vec<_> = nearby
                .iter()
                .filter(|(s, _)| distance(s.offset, median) <= CONSENSUS_RADIUS)
                .collect();
            let total = inliers.iter().map(|(_, w)| w).sum::<f64>();
            if inliers.len() < 3
                || total < nearby.iter().map(|(_, w)| w).sum::<f64>() * 0.6
                || distance(offset, median) > CONSENSUS_RADIUS
            {
                continue;
            }
            let target = bounded(
                std::array::from_fn(|axis| {
                    inliers.iter().map(|(s, w)| s.offset[axis] * w).sum::<f64>() / total
                }),
                MAX_OFFSET,
            );
            let step = bounded(
                std::array::from_fn(|axis| (target[axis] - node.offset[axis]) * 0.25),
                MAX_STEP,
            );
            node.offset = bounded(
                std::array::from_fn(|axis| node.offset[axis] + step[axis] * influence),
                MAX_OFFSET,
            );
            node.trained = true;
            agreed = true;
        }
        // Bound the spatial gradient as well as the displacement. In screen
        // pixels each partial derivative is <= 0.45, so this residual map cannot
        // fold or reverse directions, even on a small or negative-origin screen.
        for _ in 0..10 {
            if field_is_stable(&proposed, rect) {
                break;
            }
            for (node, old) in proposed.iter_mut().zip(before.iter()) {
                node.offset =
                    std::array::from_fn(|axis| (node.offset[axis] + old.offset[axis]) * 0.5);
            }
        }
        if !field_is_stable(&proposed, rect) {
            self.status = "Skipped: correction would distort map";
            return;
        }
        self.field = proposed;
        if before
            .iter()
            .zip(proposed)
            .any(|(a, b)| a.offset != b.offset)
        {
            self.updates += 1;
        }
        self.status = if agreed {
            "Learning local correction map"
        } else {
            "Waiting for consistent local clicks"
        };
    }
}
fn normalized(base: XY, rect: [i32; 4]) -> XY {
    [
        (base[0] - rect[0] as f64) / (rect[2] - rect[0]).max(1) as f64,
        (base[1] - rect[1] as f64) / (rect[3] - rect[1]).max(1) as f64,
    ]
}
fn spatial_weight(a: XY, b: XY, offset: XY, rect: [i32; 4]) -> f64 {
    // Larger errors require broader support to avoid a steep distortion around
    // one clicked pixel. Small residuals remain local on the dense grid.
    let size = (rect[2] - rect[0]).min(rect[3] - rect[1]).max(1) as f64;
    let radius = (3.8 * offset[0].hypot(offset[1]) / size).clamp(0.18, 1.5);
    let r = distance(a, b) / radius;
    if r >= 1.0 {
        0.0
    } else {
        (1.0 - r * r).powi(2)
    }
}
fn weighted_median(samples: &[(&Label, f64)]) -> XY {
    std::array::from_fn(|axis| {
        let mut values: Vec<_> = samples.iter().map(|(s, w)| (s.offset[axis], *w)).collect();
        values.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = values.iter().map(|v| v.1).sum::<f64>() * 0.5;
        let mut sum = 0.0;
        for (value, weight) in values {
            sum += weight;
            if sum >= half {
                return value;
            }
        }
        unreachable!()
    })
}
fn field_is_stable(field: &[Node; COLS * ROWS], rect: [i32; 4]) -> bool {
    let dx = (rect[2] - rect[0]) as f64 / (COLS - 1) as f64;
    let dy = (rect[3] - rect[1]) as f64 / (ROWS - 1) as f64;
    field.iter().enumerate().all(|(i, n)| {
        (i % COLS == COLS - 1 || distance(n.offset, field[i + 1].offset) <= dx * 0.45)
            && (i / COLS == ROWS - 1 || distance(n.offset, field[i + COLS].offset) <= dy * 0.45)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const RECT: [i32; 4] = [0, 0, 3840, 2160];
    fn local_offset(l: &Learner) -> XY {
        l.offset_at([1000.0, 1000.0], RECT)
    }
    #[test]
    fn three_hundred_pixel_corrections_are_accepted_and_converge() {
        let mut learner = Learner::default();
        for n in 0..400 {
            click(&mut learner, n * 500, [240., -180.]);
        }
        assert_eq!(learner.accepted, 400);
        assert!(
            distance(local_offset(&learner), [240., -180.]) < 2.0,
            "offset {:?}",
            local_offset(&learner)
        );
        assert!(field_is_stable(&learner.field, RECT));
        let mut too_far = Learner::default();
        click(&mut too_far, 0, [301., 0.]);
        assert_eq!(too_far.accepted, 0);
    }
    #[test]
    fn learned_field_survives_restart_and_context_switch_but_confirmed_reset_clears_all() {
        let path = std::env::temp_dir().join(format!(
            "gaze-restore-{}-{}.sqlite3",
            std::process::id(),
            crate::learning_log::timestamp_ms()
        ));
        let display = crate::calibration::Display {
            name: "test".into(),
            rect: RECT,
        };
        let other = crate::calibration::Display {
            name: "other".into(),
            rect: RECT,
        };
        let connect = || Learner {
            store: Some(Store::open(&path).unwrap()),
            ..Learner::default()
        };
        let expected;
        {
            let mut learner = connect();
            learner.change_context(&display, None);
            for n in 0..20 {
                click(&mut learner, n * 1000, [100., -80.]);
            }
            expected = local_offset(&learner);
            assert!(distance(expected, [0.; 2]) > 10.);
            learner.change_context(&other, None);
            assert_eq!(local_offset(&learner), [0.; 2]);
            learner.change_context(&display, None);
            assert_eq!(local_offset(&learner), expected);
        }
        {
            let mut learner = connect();
            learner.change_context(&display, None);
            assert_eq!(local_offset(&learner), expected);
            assert_eq!(learner.accepted, 20);
            assert!(!learner.labels.is_empty());
            learner.reset_persistent().unwrap();
            assert_eq!(local_offset(&learner), [0.; 2]);
        }
        {
            let mut learner = connect();
            learner.change_context(&display, None);
            assert_eq!(learner.accepted, 0);
            assert_eq!(local_offset(&learner), [0.; 2]);
        }
        std::fs::remove_file(path).unwrap();
    }
    fn click(l: &mut Learner, now: u32, error: XY) {
        let base = [1000.0, 1000.0];
        l.begin(
            now,
            base,
            [base[0] + local_offset(l)[0], base[1] + local_offset(l)[1]],
            1,
            RECT,
        );
        let target = [base[0] + error[0], base[1] + error[1]];
        l.event(now.wrapping_add(180), target, 1, 1);
        l.event(now.wrapping_add(230), target, 2, 1);
    }
    #[test]
    fn repeated_corrections_converge_without_compounding_and_each_step_is_bounded() {
        let mut l = Learner::default();
        // Peripheral nodes get smaller updates than the directly observed area.
        for n in 0..400 {
            let old = local_offset(&l);
            click(&mut l, n * 1000, [30.0, -20.0]);
            assert!(distance(old, local_offset(&l)) <= MAX_STEP + 1e-9);
            if n < 2 {
                assert_eq!(local_offset(&l), [0.0; 2]);
            }
        }
        assert!(distance(local_offset(&l), [30.0, -20.0]) < 0.1);
    }
    #[test]
    fn outlier_cannot_move_established_offset_and_recent_posture_changes_replace_old_labels() {
        let mut l = Learner::default();
        for n in 0..30 {
            click(&mut l, n * 1000, [20.0, 0.0]);
        }
        let old = local_offset(&l);
        click(&mut l, 31_000, [-70.0, 60.0]);
        assert_eq!(local_offset(&l), old);
        for n in 32..432 {
            click(&mut l, n * 1000, [-15.0, 10.0]);
        }
        assert!(distance(local_offset(&l), [-15.0, 10.0]) < 0.1);
    }
    #[test]
    fn drag_long_move_delay_scroll_outside_display_and_quick_click_are_rejected() {
        for kind in 0..6 {
            let mut l = Learner::default();
            l.begin(0, [1000.0; 2], [1000.0; 2], 1, RECT);
            match kind {
                0 => {
                    l.event(200, [1010.0; 2], 1, 1);
                    l.event(220, [1020.0; 2], 0, 1);
                }
                1 => l.event(100, [1300.0; 2], 0, 1),
                2 => l.event(MAX_CLICK_MS + 1, [1000.0; 2], 1, 1),
                3 => l.event(100, [1000.0; 2], 1024, 1),
                4 => l.event(100, [-1.0, 1000.0], 1, 2),
                _ => l.event(10, [1000.0; 2], 1, 1),
            }
            l.event(250, [1010.0; 2], 2, 1);
            assert_eq!(l.accepted, 0, "case {kind}");
            assert_eq!(local_offset(&l), [0.0; 2]);
        }
    }
    #[test]
    fn short_cross_window_corrections_and_click_opened_windows_are_eligible() {
        let mut learner = Learner::default();
        // A gaze landing just above the taskbar, followed by a correction onto
        // the taskbar and a click which opens a different top-level window.
        for n in 0..4 {
            let t = n * 4000;
            learner.begin(t, [430., 2066.], [430., 2066.], 1, RECT);
            learner.event(t + 100, [483., 2087.], 0, 2);
            assert!(learner.pending.is_some());
            learner.event(t + 2700, [483., 2087.], 1, 2);
            learner.event(t + 2850, [483., 2087.], 2, 3);
            assert!(learner.take_feedback().unwrap().accepted);
        }
        assert_eq!(learner.accepted, 4);
        assert!(learner.updates > 0);
    }
    #[test]
    fn timely_press_can_release_after_deadline_and_screen_edge_is_a_valid_target() {
        let mut learner = Learner::default();
        learner.begin(0, [20., 100.], [20., 100.], 1, RECT);
        learner.event(2900, [0., 100.], 1, 2);
        learner.expire(3100);
        learner.event(3200, [0., 100.], 2, 3);
        assert_eq!(learner.accepted, 1);
        learner.begin(4000, [20., 100.], [20., 100.], 1, RECT);
        learner.event(6900, [0., 100.], 1, 2);
        learner.event(7450, [0., 100.], 2, 3);
        assert_eq!(learner.accepted, 1);
        assert_eq!(
            learner.take_feedback().unwrap().reason,
            "Skipped: held click"
        );
    }
    #[test]
    fn offscreen_landings_preserve_the_correction_even_when_not_used_for_learning() {
        let mut learner = Learner::default();
        learner.begin(0, [100., 2300.], [100., 2159.], 1, RECT);
        let pending = learner.pending.as_ref().unwrap();
        assert_eq!(pending.base, [100., 2300.]);
        assert_eq!(pending.rejection, Some("Skipped: edge landing"));
        learner.event(100, [100., 2110.], 1, 2);
        learner.event(200, [100., 2110.], 2, 2);
        let feedback = learner.take_feedback().unwrap();
        assert_eq!(feedback.selection, Some([100., 2110.]));
        assert_eq!(feedback.reason, "Skipped: edge landing");
        assert_eq!(learner.accepted, 0);
    }
    #[test]
    fn no_correction_clicks_counter_bias_and_reset_and_freeze_work() {
        let mut l = Learner::default();
        for n in 0..30 {
            click(&mut l, n * 1000, [20.0; 2]);
        }
        l.toggle();
        let offset = local_offset(&l);
        click(&mut l, 31_000, [0.0; 2]);
        assert_eq!(local_offset(&l), offset);
        l.toggle();
        for n in 32..432 {
            click(&mut l, n * 1000, [0.0; 2]);
        }
        assert!(distance(local_offset(&l), [0.0; 2]) < 0.1);
        l.reset();
        assert_eq!(l.accepted, 0);
        assert_eq!(local_offset(&l), [0.0; 2]);
    }
    #[test]
    fn clock_wrap_and_stale_history_and_offset_limit() {
        let mut l = Learner::default();
        click(&mut l, u32::MAX - 150, [30.0; 2]);
        assert_eq!(l.accepted, 1);
        for n in 0..100 {
            click(&mut l, 1000 + n * 1000, [75.0; 2]);
        }
        assert!(distance(local_offset(&l), [0.0; 2]) <= MAX_OFFSET + 1e-9);
        let old = local_offset(&l);
        click(&mut l, 500_000, [0.0; 2]);
        assert_eq!(local_offset(&l), old);
    }

    #[test]
    fn saved_attempts_preserve_base_landing_target_outcome_and_reset_history() {
        let directory = std::env::temp_dir().join(format!(
            "gaze-attempt-test-{}-{}",
            std::process::id(),
            crate::learning_log::timestamp_ms()
        ));
        let mut l = Learner {
            recorder: Some(crate::learning_log::Recorder::start(&directory).unwrap()),
            ..Learner::default()
        };
        for n in 0..6 {
            click(&mut l, n * 1000, [20.0, 10.0]);
        }
        l.begin(7000, [1000.0; 2], [1000.0; 2], 1, RECT);
        l.event(7100, [1020.0; 2], 1024, 1);
        l.reset();
        click(&mut l, 8000, [-10.0, 0.0]);
        drop(l);
        let files: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let path = files
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .unwrap();
        let records: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(records.len(), 9);
        assert_eq!(records[0]["base"], json!([1000.0, 1000.0]));
        assert_eq!(records[0]["target"], json!([1020.0, 1010.0]));
        assert_eq!(records[0]["updated"], false);
        assert_eq!(records[4]["updated"], true);
        assert_ne!(records[5]["landing"], records[5]["base"]);
        assert_eq!(records[6]["accepted"], false);
        assert_eq!(records[7]["kind"], "reset");
        assert_eq!(records[8]["epoch"], 1);
        for file in files {
            std::fs::remove_file(file).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }

    fn pixels(p: XY, rect: [i32; 4]) -> XY {
        std::array::from_fn(|a| rect[a] as f64 + p[a] * (rect[a + 2] - rect[a]) as f64)
    }

    #[test]
    fn corner_toggle_restores_baseline_without_changing_live_learning() {
        let mut learner = Learner::default();
        let mut coefficients = vec![[0., 0.]; 18];
        coefficients[0] = [100., 100.];
        learner.corner = Some(crate::corner::Model {
            version: 1,
            context: json!({}),
            coefficients,
            training_samples: 40,
            trained_before_ms: 1,
        });
        let p = pixels([0.01, 0.01], RECT);
        let baseline = learner.offset_at(p, RECT);
        learner.toggle_corner();
        assert!(distance(learner.offset_at(p, RECT), baseline) > 100.);
        for n in 0..100 {
            learner.observe(n * 100, [0.5, 0.5], [20., -10.], RECT);
        }
        let updates = learner.updates;
        assert!(updates > 0);
        let live = learner.offset_at(pixels([0.5, 0.5], RECT), RECT);
        learner.toggle_corner();
        assert_eq!(learner.offset_at(p, RECT), baseline);
        assert_eq!(learner.offset_at(pixels([0.5, 0.5], RECT), RECT), live);
        assert_eq!(learner.updates, updates);
        learner.reset();
        assert!(!learner.corner_enabled);
    }

    #[test]
    fn opposite_horizontal_and_vertical_errors_learn_independently() {
        let mut l = Learner::default();
        let positions = [
            [1.0 / 6.0, 0.0],
            [5.0 / 6.0, 0.0],
            [1.0 / 6.0, 1.0],
            [5.0 / 6.0, 1.0],
        ];
        let errors = [[25.0, 30.0], [-25.0, 30.0], [25.0, -30.0], [-25.0, -30.0]];
        for n in 0..600 {
            let i = n as usize % 4;
            l.observe(n * 100, positions[i], errors[i], RECT);
        }
        for (p, expected) in positions.into_iter().zip(errors) {
            assert!(distance(l.offset_at(pixels(p, RECT), RECT), expected) < 0.1);
        }
        assert_eq!(l.offset_at(pixels([0.5, 0.5], RECT), RECT), [0.0; 2]);
    }

    #[test]
    fn interpolation_is_continuous_bounded_and_independent_of_monitor_origin() {
        let mut l = Learner::default();
        let shifted = [-3840, -2160, 0, 0];
        for n in 0..100 {
            let old = l.field;
            l.observe(n * 100, [0.5, 0.5], [50.0, -30.0], RECT);
            for row in 0..21 {
                for col in 0..31 {
                    let p = [col as f64 / 30.0, row as f64 / 20.0];
                    let current = l.offset_at(pixels(p, RECT), RECT);
                    let previous = Learner {
                        field: old,
                        ..Learner::default()
                    }
                    .offset_at(pixels(p, RECT), RECT);
                    assert!(distance(current, previous) <= MAX_STEP + 1e-9);
                    assert!(distance(current, [0.0; 2]) <= MAX_OFFSET + 1e-9);
                    assert!(distance(current, l.offset_at(pixels(p, shifted), shifted)) < 1e-9);
                }
            }
        }
        for row in 1..ROWS - 1 {
            for col in 1..COLS - 1 {
                let p = pixels(
                    [
                        col as f64 / (COLS - 1) as f64,
                        row as f64 / (ROWS - 1) as f64,
                    ],
                    RECT,
                );
                for axis in 0..2 {
                    let mut a = p;
                    let mut b = p;
                    a[axis] -= 1e-6;
                    b[axis] += 1e-6;
                    assert!(distance(l.offset_at(a, RECT), l.offset_at(b, RECT)) < 1e-5);
                }
            }
        }
        let small = [0, 0, 320, 240];
        // Use a fresh model for another display, as the controller does.
        let mut small_model = Learner::default();
        for n in 0..200 {
            small_model.observe(n * 100, [0.5, 0.5], [75.0, 0.0], small);
            assert!(field_is_stable(&small_model.field, small));
        }
    }

    #[test]
    fn curved_asymmetric_error_improves_at_held_out_positions() {
        fn error([x, y]: XY) -> XY {
            [
                28.0 * (x - 0.5) + 12.0 * y * y,
                32.0 * (y - 0.5) + 16.0 * x * y,
            ]
        }
        let mut l = Learner::default();
        for n in 0..4200 {
            let i = n as usize % 35;
            let p = [(i % 7) as f64 / 6.0, (i / 7) as f64 / 4.0];
            l.observe(n * 50, p, error(p), RECT);
        }
        let mut before = 0.0;
        let mut after = 0.0;
        for row in 0..ROWS - 1 {
            for col in 0..COLS - 1 {
                let p = [
                    (col as f64 + 0.5) / (COLS - 1) as f64,
                    (row as f64 + 0.5) / (ROWS - 1) as f64,
                ];
                before += distance(error(p), [0.0; 2]).powi(2);
                after += distance(error(p), l.offset_at(pixels(p, RECT), RECT)).powi(2);
            }
        }
        assert!(
            after < before * 0.25,
            "held-out squared errors: {before} -> {after}"
        );
    }
    #[test]
    fn feedback_matches_actual_learning_and_consuming_it_does_not_change_the_map() {
        let mut shown = Learner::default();
        let mut hidden = Learner::default();
        for n in 0..8 {
            let old = local_offset(&shown);
            click(&mut shown, n * 1000, [30., -20.]);
            click(&mut hidden, n * 1000, [30., -20.]);
            let event = shown.take_feedback().unwrap();
            assert!(event.accepted && !event.demo);
            assert_eq!(event.updated, n >= 2);
            assert!(distance(event.before, [1000. + old[0], 1000. + old[1]]) < 1e-9);
            let new = local_offset(&shown);
            assert!(distance(event.after, [1000. + new[0], 1000. + new[1]]) < 1e-9);
            assert_eq!(event.selection, Some([1030., 980.]));
            assert!(shown.take_feedback().is_none());
            assert_eq!(local_offset(&shown), local_offset(&hidden));
            assert_eq!(shown.updates, hidden.updates);
        }
        shown.begin(9000, [1000.; 2], [1000.; 2], 1, RECT);
        shown.event(9100, [1300.; 2], 0, 1);
        assert!(shown.take_feedback().is_none());
        shown.event(9200, [1300.; 2], 1, 1);
        assert!(shown.take_feedback().is_none());
        shown.event(9250, [1300.; 2], 2, 1);
        let rejected = shown.take_feedback().unwrap();
        assert!(!rejected.accepted && !rejected.updated);
        assert_eq!(rejected.selection, Some([1300.; 2]));
        assert_eq!(rejected.after, rejected.before);
    }
    #[test]
    fn rejected_movement_and_expiry_are_silent_until_a_completed_click() {
        for end in 0..4 {
            let mut learner = Learner::default();
            learner.begin(0, [1000.; 2], [1000.; 2], 1, RECT);
            learner.event(100, [1300.; 2], 0, 1);
            assert!(learner.take_feedback().is_none());
            // Returning near the landing must not rehabilitate a long movement.
            learner.event(200, [1010.; 2], 0, 1);
            match end {
                0 => learner.expire(MAX_CLICK_MS + 1),
                1 => learner.begin(300, [1000.; 2], [1000.; 2], 1, RECT),
                2 => learner.event(300, [1010.; 2], 2, 1), // release without press
                _ => {
                    learner.event(300, [1010.; 2], 1, 1);
                    assert!(learner.take_feedback().is_none());
                    learner.event(350, [1010.; 2], 2, 1);
                    let event = learner.take_feedback().unwrap();
                    assert_eq!(event.reason, "Skipped: long correction");
                    assert!(!event.accepted && !event.updated);
                }
            }
            assert!(learner.take_feedback().is_none());
            assert_eq!(learner.accepted, 0);
            assert_eq!(learner.updates, 0);
        }
    }
}
