//! App-local residual calibration. Models are selected using leave-one-target-out
//! training error, then evaluated once on a separate, untouched validation block.
//! Per-axis median/MAD rejection does NOT use proximity to the intended target.
//! Fit and apply use the same bounded basis, including raw gaze beyond an edge.
use crate::tobii::Sample;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SETTLE: Duration = Duration::from_millis(800);
const COLLECT: Duration = Duration::from_millis(1600);
pub const TRAIN_COUNT: usize = 25;
pub const CHECK_TARGETS: [XY; 12] = [
    [0.07, 0.07],
    [0.93, 0.93],
    [0.93, 0.07],
    [0.07, 0.93],
    [0.50, 0.07],
    [0.50, 0.93],
    [0.07, 0.50],
    [0.93, 0.50],
    [0.30, 0.30],
    [0.70, 0.70],
    [0.70, 0.30],
    [0.30, 0.70],
];
pub const CHECK_COUNT: usize = CHECK_TARGETS.len();
pub const TOTAL: usize = TRAIN_COUNT + CHECK_COUNT;
type XY = [f64; 2];

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub name: String,
    pub rect: [i32; 4],
}
impl Display {
    pub fn size(&self) -> XY {
        [
            (self.rect[2] - self.rect[0]) as f64,
            (self.rect[3] - self.rect[1]) as f64,
        ]
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Observation {
    pub xy: XY,
    pub valid: bool,
    pub timestamp_us: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Capture {
    pub target: XY,
    pub mean: XY,
    pub retained: Vec<XY>,
    pub rejected: Vec<XY>,
    pub observations: Vec<Observation>,
    pub scatter_px: f64,
    pub drift_px: f64,
}
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) * 0.5
    } else {
        values[n / 2]
    }
}
fn mean(points: &[XY]) -> XY {
    let mut sum = [0.0; 2];
    for p in points {
        sum[0] += p[0];
        sum[1] += p[1];
    }
    [sum[0] / points.len() as f64, sum[1] / points.len() as f64]
}
fn distance(a: XY, b: XY, size: XY) -> f64 {
    ((a[0] - b[0]) * size[0]).hypot((a[1] - b[1]) * size[1])
}
pub fn summarize(target: XY, observations: Vec<Observation>, size: XY) -> Result<Capture, String> {
    let points: Vec<_> = observations
        .iter()
        .filter(|o| o.valid && o.xy.iter().all(|x| x.is_finite()))
        .map(|o| o.xy)
        .collect();
    if points.len() < 25 || points.len() as f64 / (observations.len().max(1) as f64) < 0.65 {
        return Err(
            "Tracking lost here. Adjust tracker position in your normal posture; Space retries."
                .into(),
        );
    }
    let center = [
        median(points.iter().map(|p| p[0]).collect()),
        median(points.iter().map(|p| p[1]).collect()),
    ];
    // Modified Z score 0.67449*(x-median)/MAD, threshold 3.5. A 2-pixel
    // MAD floor handles quantized/identical samples without dividing by zero.
    let threshold: XY = std::array::from_fn(|axis| {
        let mad = median(
            points
                .iter()
                .map(|p| (p[axis] - center[axis]).abs())
                .collect(),
        );
        3.5 / 0.67448975 * mad.max(2.0 / size[axis])
    });
    let (retained, rejected): (Vec<_>, Vec<_>) = points
        .into_iter()
        .partition(|p| (0..2).all(|axis| (p[axis] - center[axis]).abs() <= threshold[axis]));
    if retained.len() < 25
        || retained.len() as f64 / ((retained.len() + rejected.len()) as f64) < 0.7
    {
        return Err(
            "Too many outliers. Hold your gaze on the dot and press Space to retry.".into(),
        );
    }
    let avg = mean(&retained);
    let scatter = (retained
        .iter()
        .map(|p| distance(*p, avg, size).powi(2))
        .sum::<f64>()
        / retained.len() as f64)
        .sqrt();
    let third = retained.len() / 3;
    let drift = distance(
        mean(&retained[..third]),
        mean(&retained[retained.len() - third..]),
        size,
    );
    let diagonal = size[0].hypot(size[1]);
    if scatter > diagonal * 0.025 || drift > diagonal * 0.02 {
        return Err("Gaze moved during capture. Look steadily at the dot; Space retries.".into());
    }
    Ok(Capture {
        target,
        mean: avg,
        retained,
        rejected,
        observations,
        scatter_px: scatter,
        drift_px: drift,
    })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Model {
    pub kind: String,
    pub coefficients: [Vec<f64>; 2],
    pub training_cv_rms_px: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalMap>,
}
/// Smooth Gaussian residuals around measured gaze positions, on top of an
/// affine baseline. Coordinates are normalized per screen axis, not pixels.
#[derive(Clone, Serialize, Deserialize)]
pub struct LocalMap {
    pub centers: Vec<XY>,
    pub weights: [Vec<f64>; 2],
    pub bandwidth: f64,
    pub ridge: f64,
}
fn kernel(a: XY, b: XY, bandwidth: f64) -> f64 {
    (-((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)) / (2.0 * bandwidth * bandwidth)).exp()
}
fn features(p: XY) -> [f64; 6] {
    let x = p[0].clamp(0.0, 1.0) - 0.5;
    let y = p[1].clamp(0.0, 1.0) - 0.5;
    [1.0, x, y, x * x, x * y, y * y]
}
impl Model {
    pub fn apply(&self, p: XY) -> XY {
        // Keep the correction bounded outside the calibrated display; do not
        // clamp the gaze itself or make off-screen gaze appear on an edge.
        let basis = features(p);
        std::array::from_fn(|axis| {
            p[axis]
                + self.coefficients[axis]
                    .iter()
                    .zip(basis)
                    .map(|(c, b)| c * b)
                    .sum::<f64>()
                + self.local.as_ref().map_or(0.0, |local| {
                    let bounded = p.map(|v| v.clamp(0.0, 1.0));
                    local
                        .centers
                        .iter()
                        .zip(&local.weights[axis])
                        .map(|(center, weight)| weight * kernel(bounded, *center, local.bandwidth))
                        .sum::<f64>()
                })
        })
    }
    fn stable(&self) -> bool {
        if let Some(local) = &self.local {
            if !(0.08..=1.0).contains(&local.bandwidth)
                || !(0.001..=10.0).contains(&local.ridge)
                || local.centers.is_empty()
                || local.centers.len() > 100
                || local
                    .centers
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                || local
                    .weights
                    .iter()
                    .any(|w| w.len() != local.centers.len() || w.iter().any(|v| !v.is_finite()))
            {
                return false;
            }
        }
        for yi in 0..=20 {
            for xi in 0..=20 {
                let p = [xi as f64 / 20.0, yi as f64 / 20.0];
                let q = self.apply(p);
                if q.iter().any(|v| !v.is_finite()) || distance(p, q, [1.0, 1.0]) > 0.25 {
                    return false;
                }
                // Interior finite differences test that the fitted map does not fold
                // or grossly stretch/shrink the display.
                let p = [p[0].clamp(0.001, 0.999), p[1].clamp(0.001, 0.999)];
                let q = self.apply(p);
                let dx = self.apply([p[0] + 0.0001, p[1]]);
                let dy = self.apply([p[0], p[1] + 0.0001]);
                let a = (dx[0] - q[0]) * 10000.0;
                let b = (dy[0] - q[0]) * 10000.0;
                let c = (dx[1] - q[1]) * 10000.0;
                let d = (dy[1] - q[1]) * 10000.0;
                let det = a * d - b * c;
                if !(0.4..=2.2).contains(&det)
                    || !(0.45..=1.8).contains(&a)
                    || !(0.45..=1.8).contains(&d)
                {
                    return false;
                }
            }
        }
        true
    }
}
fn solve(mut matrix: Vec<Vec<f64>>, mut rhs: Vec<f64>) -> Option<Vec<f64>> {
    let n = rhs.len();
    for col in 0..n {
        let pivot =
            (col..n).max_by(|a, b| matrix[*a][col].abs().total_cmp(&matrix[*b][col].abs()))?;
        if matrix[pivot][col].abs() < 1e-10 {
            return None;
        }
        matrix.swap(col, pivot);
        rhs.swap(col, pivot);
        let div = matrix[col][col];
        for value in &mut matrix[col][col..] {
            *value /= div;
        }
        rhs[col] /= div;
        let pivot_row = matrix[col].clone();
        for row in 0..n {
            if row != col {
                let factor = matrix[row][col];
                for (value, pivot) in matrix[row][col..].iter_mut().zip(&pivot_row[col..]) {
                    *value -= factor * pivot;
                }
                rhs[row] -= factor * rhs[col];
            }
        }
    }
    Some(rhs)
}
fn fit(captures: &[&Capture], terms: usize) -> Option<Model> {
    let mut matrix = vec![vec![0.0; terms]; terms];
    let mut rhs = [vec![0.0; terms], vec![0.0; terms]];
    for cap in captures {
        let basis = features(cap.mean);
        for i in 0..terms {
            for j in 0..terms {
                matrix[i][j] += basis[i] * basis[j];
            }
            for (axis, values) in rhs.iter_mut().enumerate() {
                values[i] += basis[i] * (cap.target[axis] - cap.mean[axis]);
            }
        }
    }
    // Ridge shrinks the residual field toward the identity map, particularly its
    // curvature. Each target has equal weight regardless of sample count.
    for (i, row) in matrix.iter_mut().enumerate().skip(1) {
        row[i] += if i >= 3 { 0.01 } else { 0.000001 };
    }
    let coefficients = [
        solve(matrix.clone(), rhs[0].clone())?,
        solve(matrix, rhs[1].clone())?,
    ];
    let model = Model {
        kind: if terms == 3 { "Affine" } else { "Quadratic" }.into(),
        coefficients,
        training_cv_rms_px: 0.0,
        local: None,
    };
    model.stable().then_some(model)
}
fn candidate(captures: &[Capture], terms: usize, size: XY) -> Option<Model> {
    let mut error = 0.0;
    for held in 0..captures.len() {
        let training: Vec<_> = captures
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != held)
            .map(|(_, c)| c)
            .collect();
        let model = fit(&training, terms)?;
        error += distance(
            model.apply(captures[held].mean),
            captures[held].target,
            size,
        )
        .powi(2);
    }
    let mut model = fit(&captures.iter().collect::<Vec<_>>(), terms)?;
    model.training_cv_rms_px = (error / captures.len() as f64).sqrt();
    Some(model)
}
fn fit_local(captures: &[&Capture], bandwidth: f64, ridge: f64) -> Option<Model> {
    let mut model = fit(captures, 3)?;
    let centers: Vec<_> = captures
        .iter()
        .map(|c| c.mean.map(|v| v.clamp(0.0, 1.0)))
        .collect();
    let mut matrix: Vec<Vec<f64>> = centers
        .iter()
        .map(|a| centers.iter().map(|b| kernel(*a, *b, bandwidth)).collect())
        .collect();
    for (i, row) in matrix.iter_mut().enumerate() {
        row[i] += ridge;
    }
    let residuals: Vec<_> = captures
        .iter()
        .map(|c| {
            let base = model.apply(c.mean);
            [c.target[0] - base[0], c.target[1] - base[1]]
        })
        .collect();
    let weights = [
        solve(matrix.clone(), residuals.iter().map(|r| r[0]).collect())?,
        solve(matrix, residuals.iter().map(|r| r[1]).collect())?,
    ];
    model.kind = "Local".into();
    model.local = Some(LocalMap {
        centers,
        weights,
        bandwidth,
        ridge,
    });
    model.stable().then_some(model)
}
fn local_candidate(captures: &[Capture], size: XY, bandwidth: f64, ridge: f64) -> Option<Model> {
    let mut error = 0.0;
    for held in 0..captures.len() {
        let training: Vec<_> = captures
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != held)
            .map(|(_, c)| c)
            .collect();
        // Refit BOTH the affine baseline and local field without the held target.
        let model = fit_local(&training, bandwidth, ridge)?;
        error += distance(
            model.apply(captures[held].mean),
            captures[held].target,
            size,
        )
        .powi(2);
    }
    let mut model = fit_local(&captures.iter().collect::<Vec<_>>(), bandwidth, ridge)?;
    model.training_cv_rms_px = (error / captures.len() as f64).sqrt();
    Some(model)
}
pub fn choose_model(captures: &[Capture], size: XY) -> Option<Model> {
    if captures.len() < 8 {
        return None;
    }
    let affine = candidate(captures, 3, size)?;
    let global = match candidate(captures, 6, size) {
        Some(q)
            if q.training_cv_rms_px < affine.training_cv_rms_px * 0.9
                && affine.training_cv_rms_px - q.training_cv_rms_px > 1.0 =>
        {
            q
        }
        _ => affine,
    };
    let local = local_model(captures, size);
    Some(match local {
        // Ridge already controls local complexity. Choose the lower training
        // prediction error, retaining the simpler map for numerical ties.
        Some(m) if m.training_cv_rms_px < global.training_cv_rms_px - 0.001 => m,
        _ => global,
    })
}
pub fn local_model(captures: &[Capture], size: XY) -> Option<Model> {
    if captures.len() < 8 {
        return None;
    }
    // Fixed search space; validation captures never influence model selection.
    let mut local: Option<Model> = None;
    for bandwidth in [0.12, 0.20, 0.30, 0.45] {
        for ridge in [0.03, 0.15, 0.60] {
            if let Some(m) = local_candidate(captures, size, bandwidth, ridge) {
                if local
                    .as_ref()
                    .is_none_or(|best| m.training_cv_rms_px < best.training_cv_rms_px)
                {
                    local = Some(m);
                }
            }
        }
    }
    local
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CornerError {
    pub name: String,
    pub raw_px: f64,
    pub corrected_px: f64,
}
fn corner_name(p: XY) -> Option<&'static str> {
    match (p[0], p[1]) {
        (x, y) if x <= 0.15 && y <= 0.15 => Some("Upper left"),
        (x, y) if x >= 0.85 && y <= 0.15 => Some("Upper right"),
        (x, y) if x <= 0.15 && y >= 0.85 => Some("Lower left"),
        (x, y) if x >= 0.85 && y >= 0.85 => Some("Lower right"),
        _ => None,
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Metrics {
    // Older reports remain readable, but must not imply that corners were checked.
    #[serde(default)]
    pub corner_errors: Vec<CornerError>,
    pub raw_mean_px: f64,
    pub corrected_mean_px: f64,
    pub raw_sample_rms_px: f64,
    pub corrected_sample_rms_px: f64,
    pub raw_worst_target_px: f64,
    pub corrected_worst_target_px: f64,
    pub improved_targets: usize,
    pub recommend: bool,
}
pub fn evaluate(model: &Model, captures: &[Capture], size: XY) -> Metrics {
    let mut raw = 0.0;
    let mut corrected = 0.0;
    let mut raw_rms = 0.0;
    let mut corrected_rms = 0.0;
    let mut worst_raw: f64 = 0.0;
    let mut worst_corrected: f64 = 0.0;
    let mut improved = 0;
    let mut corner_errors = Vec::new();
    for cap in captures {
        let r = distance(cap.mean, cap.target, size);
        let c = distance(model.apply(cap.mean), cap.target, size);
        raw += r;
        corrected += c;
        worst_raw = worst_raw.max(r);
        worst_corrected = worst_corrected.max(c);
        improved += usize::from(c < r);
        if let Some(name) = corner_name(cap.target) {
            corner_errors.push(CornerError {
                name: name.into(),
                raw_px: r,
                corrected_px: c,
            });
        }
        // All valid samples count for the generalization metric, including
        // samples rejected from centroid estimation. No cherry-picking the tail.
        let valid: Vec<_> = cap
            .observations
            .iter()
            .filter(|o| o.valid && o.xy.iter().all(|v| v.is_finite()))
            .collect();
        raw_rms += valid
            .iter()
            .map(|o| distance(o.xy, cap.target, size).powi(2))
            .sum::<f64>()
            / valid.len() as f64;
        corrected_rms += valid
            .iter()
            .map(|o| distance(model.apply(o.xy), cap.target, size).powi(2))
            .sum::<f64>()
            / valid.len() as f64;
    }
    let n = captures.len() as f64;
    let raw_mean = raw / n;
    let corrected_mean = corrected / n;
    let raw_rms = (raw_rms / n).sqrt();
    let corrected_rms = (corrected_rms / n).sqrt();
    let recommend = corrected_mean < raw_mean * 0.9
        && raw_mean - corrected_mean > 2.0
        && corrected_rms < raw_rms
        && worst_corrected <= worst_raw * 1.1
        && improved * 4 >= captures.len() * 3
        && corner_errors
            .iter()
            .all(|c| c.corrected_px <= c.raw_px + (c.raw_px * 0.1).max(2.0));
    Metrics {
        corner_errors,
        raw_mean_px: raw_mean,
        corrected_mean_px: corrected_mean,
        raw_sample_rms_px: raw_rms,
        corrected_sample_rms_px: corrected_rms,
        raw_worst_target_px: worst_raw,
        corrected_worst_target_px: worst_corrected,
        improved_targets: improved,
        recommend,
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub display: Display,
    pub model: Model,
    pub metrics: Metrics,
    pub training: Vec<Capture>,
    pub validation: Vec<Capture>,
    pub saved_at_unix_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refit_from_unix_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_metrics: Option<Metrics>,
}
impl Report {
    pub fn refit(&self) -> Result<Self, String> {
        if self.validation.is_empty() {
            return Err("No independent checks were saved.".into());
        }
        let model = choose_model(&self.training, self.display.size())
            .ok_or("Saved points could not produce a stable map.")?;
        let mut result = self.clone();
        result.schema_version = 2;
        result.metrics = evaluate(&model, &self.validation, self.display.size());
        result.model = model;
        result.refit_from_unix_ms = Some(self.saved_at_unix_ms);
        result.previous_metrics = Some(self.metrics.clone());
        result.saved_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Ok(result)
    }
    pub fn load_latest() -> Option<Self> {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("recordings");
        std::fs::read_dir(directory)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("calibration-"))
            .filter_map(|e| serde_json::from_slice::<Self>(&std::fs::read(e.path()).ok()?).ok())
            .filter(|r| {
                matches!(r.schema_version, 1 | 2)
                    && r.model
                        .coefficients
                        .iter()
                        .all(|c| matches!(c.len(), 3 | 6) && c.iter().all(|x| x.is_finite()))
                    && r.model.coefficients[0].len() == r.model.coefficients[1].len()
                    && r.model.stable()
                    && r.display.size().iter().all(|v| *v > 0.0)
            })
            .max_by_key(|r| r.saved_at_unix_ms)
    }
    pub fn save(&self) -> Result<std::path::PathBuf, String> {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("recordings");
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = directory.join(format!("calibration-{}.json", self.saved_at_unix_ms));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(file, self).map_err(|e| e.to_string())?;
        Ok(path)
    }
}
pub struct Session {
    pub display: Display,
    pub captures: Vec<Capture>,
    pub model: Option<Model>,
    pub notice: String,
    pub finished: Option<Report>,
    started: Option<Instant>,
    observations: Vec<Observation>,
    last_timestamp: Option<i64>,
}
impl Session {
    pub fn new(display: Display) -> Self {
        Self {
            display,
            captures: Vec::new(),
            model: None,
            notice: "Look at the bright dot. Press Space and hold your gaze for 2.4 seconds."
                .into(),
            finished: None,
            started: None,
            observations: Vec::new(),
            last_timestamp: None,
        }
    }
    pub fn target(&self) -> XY {
        // Scramble grid traversal to reduce a simple time-vs-position drift trend.
        // Full-display coverage plus a second band near each edge. Corner pairs
        // occur early; later targets alternate across the display to limit drift.
        const AXIS: [f64; 5] = [0.04, 0.18, 0.50, 0.82, 0.96];
        const ORDER: [usize; TRAIN_COUNT] = [
            12, 0, 24, 4, 20, 6, 18, 8, 16, 2, 22, 10, 14, 1, 23, 3, 21, 5, 19, 9, 15, 7, 17, 11,
            13,
        ];
        let i = self.captures.len();
        if i < TRAIN_COUNT {
            let j = ORDER[i];
            [AXIS[j % 5], AXIS[j / 5]]
        } else {
            CHECK_TARGETS[(i - TRAIN_COUNT).min(CHECK_COUNT - 1)]
        }
    }
    pub fn start_capture(&mut self) {
        if self.started.is_none()
            && self.finished.is_none()
            && (self.captures.len() < TRAIN_COUNT || self.model.is_some())
        {
            self.started = Some(Instant::now());
            self.observations.clear();
            self.last_timestamp = None;
            self.notice = "Settle on the center…".into();
        }
    }
    pub fn capturing(&self) -> bool {
        self.started.is_some()
    }
    pub fn progress(&self, now: Instant) -> f32 {
        self.started
            .map(|t| {
                (now.duration_since(t).as_secs_f32() / (SETTLE + COLLECT).as_secs_f32()).min(1.0)
            })
            .unwrap_or(0.0)
    }
    pub fn pause(&mut self) {
        if self.started.take().is_some() {
            self.observations.clear();
            self.notice =
                "Capture paused. Return to this window, look at the dot, then press Space.".into();
        }
    }
    pub fn tick(&mut self, samples: &[Sample], now: Instant) {
        let Some(start) = self.started else {
            return;
        };
        let end = start + SETTLE + COLLECT;
        for s in samples {
            if s.received < start + SETTLE
                || s.received > end
                || self
                    .last_timestamp
                    .is_some_and(|last| s.timestamp_us <= last)
            {
                continue;
            }
            self.last_timestamp = Some(s.timestamp_us);
            self.observations.push(Observation {
                xy: [s.xy[0] as f64, s.xy[1] as f64],
                valid: s.valid,
                timestamp_us: s.timestamp_us,
            });
        }
        if now < end {
            if now >= start + SETTLE {
                self.notice = "Collecting — keep looking at the center.".into();
            }
            return;
        }
        self.started = None;
        match summarize(
            self.target(),
            std::mem::take(&mut self.observations),
            self.display.size(),
        ) {
            Err(reason) => self.notice = reason,
            Ok(cap) => {
                self.notice = format!(
                    "Kept {} samples, rejected {}. Look at the next dot; Space when ready.",
                    cap.retained.len(),
                    cap.rejected.len()
                );
                self.captures.push(cap);
                if self.captures.len() == TRAIN_COUNT {
                    self.model = choose_model(&self.captures, self.display.size());
                    if self.model.is_none() {
                        self.notice =
                            "Could not fit a stable map. Press Esc, then C to try again.".into();
                        return;
                    }
                    self.notice=format!("Map fitted. Now check {CHECK_COUNT} NEW dots, including the corners. Space when ready.");
                }
                if self.captures.len() == TOTAL {
                    if let Some(model) = self.model.clone() {
                        let validation = self.captures[TRAIN_COUNT..].to_vec();
                        let metrics = evaluate(&model, &validation, self.display.size());
                        self.finished = Some(Report {
                            schema_version: 2,
                            display: self.display.clone(),
                            model,
                            metrics,
                            training: self.captures[..TRAIN_COUNT].to_vec(),
                            validation,
                            saved_at_unix_ms: SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis(),
                            refit_from_unix_ms: None,
                            previous_metrics: None,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn local_fixture() -> Vec<Capture> {
        let mut session = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        for _ in 0..TRAIN_COUNT {
            let raw = session.target();
            let target = local_truth(raw);
            session
                .captures
                .push(capture(target, [raw[0] - target[0], raw[1] - target[1]]));
        }
        session.captures
    }
    fn local_truth(raw: XY) -> XY {
        let x = raw[0] * std::f64::consts::TAU;
        let y = raw[1] * std::f64::consts::TAU;
        [
            raw[0] + 0.025 * x.sin() * y.cos(),
            raw[1] + 0.02 * x.cos() * y.sin(),
        ]
    }
    #[test]
    fn local_field_generalizes_nonquadratic_error_and_roundtrips() {
        let training = local_fixture();
        let local = choose_model(&training, [3840., 2160.]).unwrap();
        assert_eq!(local.kind, "Local");
        let global = candidate(&training, 6, [3840., 2160.]).unwrap();
        let mut local_error = 0.;
        let mut global_error = 0.;
        for y in [0.1, 0.35, 0.65, 0.9] {
            for x in [0.1, 0.35, 0.65, 0.9] {
                let p = [x, y];
                local_error += distance(local.apply(p), local_truth(p), [3840., 2160.]);
                global_error += distance(global.apply(p), local_truth(p), [3840., 2160.]);
            }
        }
        assert!(
            local_error < global_error * 0.8,
            "local {local_error}, global {global_error}"
        );
        let serialized = serde_json::to_string(&local).unwrap();
        let restored: Model = serde_json::from_str(&serialized).unwrap();
        assert!(restored.stable());
        assert!(
            distance(
                local.apply([0.31, 0.72]),
                restored.apply([0.31, 0.72]),
                [3840., 2160.]
            ) < 1e-8
        );
        let edge = local.apply([1., 0.5]);
        let outside = local.apply([1.2, 0.5]);
        assert!((outside[0] - edge[0] - 0.2).abs() < 1e-10);
        assert!((outside[1] - edge[1]).abs() < 1e-10);
        let mut broken = restored;
        broken.local.as_mut().unwrap().weights[0].pop();
        assert!(!broken.stable());
    }
    #[test]
    fn saved_refit_never_trains_on_checks_and_preserves_original() {
        let training = local_fixture();
        let model = candidate(&training, 3, [3840., 2160.]).unwrap();
        let validation = vec![capture([0.3, 0.3], [0.01, 0.01])];
        let mut report = Report {
            schema_version: 1,
            display: Display {
                name: "test".into(),
                rect: [0, 0, 3840, 2160],
            },
            metrics: evaluate(&model, &validation, [3840., 2160.]),
            model,
            training,
            validation,
            saved_at_unix_ms: 123,
            refit_from_unix_ms: None,
            previous_metrics: None,
        };
        let original = serde_json::to_string(&report).unwrap();
        let first = report.refit().unwrap();
        assert_eq!(original, serde_json::to_string(&report).unwrap());
        report.validation = vec![capture([0.8, 0.8], [-0.08, 0.04])];
        let second = report.refit().unwrap();
        assert_eq!(
            serde_json::to_string(&first.model).unwrap(),
            serde_json::to_string(&second.model).unwrap()
        );
        assert_eq!(first.schema_version, 2);
        assert_eq!(first.refit_from_unix_ms, Some(123));
        assert!(first.previous_metrics.is_some());
        assert_ne!(first.metrics.raw_mean_px, second.metrics.raw_mean_px);
    }
    #[test]
    fn old_polynomial_models_load_without_local_fields() {
        let json =
            r#"{"kind":"Affine","coefficients":[[0.02,0,0],[0,0,0]],"training_cv_rms_px":10}"#;
        let model: Model = serde_json::from_str(json).unwrap();
        assert!(model.local.is_none());
        assert!(model.stable());
        assert_eq!(model.apply([0.5, 0.5]), [0.52, 0.5]);
    }
    #[test]
    fn layout_reaches_every_corner_and_checks_them_independently() {
        let mut s = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        for _ in 0..TRAIN_COUNT {
            s.captures.push(capture(s.target(), [0.; 2]));
        }
        for corner in [[0.04, 0.04], [0.96, 0.04], [0.04, 0.96], [0.96, 0.96]] {
            assert!(s.captures.iter().any(|c| c.target == corner));
        }
        for right in [false, true] {
            assert_eq!(
                s.captures
                    .iter()
                    .filter(|c| c.target[1] <= 0.18
                        && if right {
                            c.target[0] >= 0.82
                        } else {
                            c.target[0] <= 0.18
                        })
                    .count(),
                4
            );
        }
        for (i, c) in s.captures.iter().enumerate() {
            assert!(s.captures[..i].iter().all(|p| p.target != c.target));
        }
        assert_eq!(
            CHECK_TARGETS
                .iter()
                .filter(|p| corner_name(**p).is_some())
                .count(),
            4
        );
        assert!(CHECK_TARGETS
            .iter()
            .all(|p| s.captures.iter().all(|c| c.target != *p)));
    }
    #[test]
    fn a_good_average_cannot_hide_a_worse_upper_corner() {
        let model = Model {
            kind: "Affine".into(),
            coefficients: [vec![-0.02, 0., 0.], vec![0.; 3]],
            training_cv_rms_px: 0.,
            local: None,
        };
        let captures: Vec<_> = CHECK_TARGETS
            .iter()
            .enumerate()
            .map(|(i, p)| capture(*p, [if i == 0 { -0.002 } else { 0.03 }, 0.]))
            .collect();
        let m = evaluate(&model, &captures, [3840., 2160.]);
        assert!(m.corrected_mean_px < m.raw_mean_px * 0.9);
        assert!(m.corrected_sample_rms_px < m.raw_sample_rms_px);
        assert!(m.corrected_worst_target_px < m.raw_worst_target_px * 1.1);
        assert_eq!(m.improved_targets, CHECK_COUNT - 1);
        assert!(m.corner_errors[0].corrected_px > m.corner_errors[0].raw_px + 2.);
        assert!(!m.recommend);
    }
    #[test]
    fn fitting_uses_the_same_basis_as_application_for_offscreen_raw_gaze() {
        let truth = Model {
            kind: "Affine".into(),
            coefficients: [vec![0.04, 0.02, 0.01], vec![-0.03, 0.01, -0.02]],
            training_cv_rms_px: 0.,
            local: None,
        };
        let mut captures = Vec::new();
        for y in [-0.03, 0.3, 0.7, 1.03] {
            for x in [-0.03, 0.3, 0.7, 1.03] {
                let target = truth.apply([x, y]);
                captures.push(capture(target, [x - target[0], y - target[1]]));
            }
        }
        let fitted = fit(&captures.iter().collect::<Vec<_>>(), 3).unwrap();
        for c in captures {
            assert!(distance(fitted.apply(c.mean), c.target, [3840., 2160.]) < 1.);
        }
    }
    #[test]
    fn missing_gaze_cannot_advance_a_calibration_target() {
        let mut s = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        let target = s.target();
        s.start_capture();
        s.tick(&[], s.started.unwrap() + SETTLE + COLLECT);
        assert!(s.captures.is_empty());
        assert_eq!(s.target(), target);
        assert!(s.notice.contains("Tracking lost"));
        assert!(!s.capturing());
    }
    #[test]
    fn older_reports_do_not_claim_corner_measurements() {
        let model = Model {
            kind: "Affine".into(),
            coefficients: [vec![-0.03, 0., 0.], vec![0.; 3]],
            training_cv_rms_px: 0.,
            local: None,
        };
        let metrics = evaluate(&model, &[capture([0.3, 0.3], [0.03, 0.])], [3840., 2160.]);
        let mut json = serde_json::to_value(metrics).unwrap();
        json.as_object_mut().unwrap().remove("corner_errors");
        let loaded: Metrics = serde_json::from_value(json).unwrap();
        assert!(loaded.corner_errors.is_empty());
    }
    #[test]
    fn complete_session_keeps_validation_separate_and_freezes_the_fit() {
        let mut session = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        let mut fitted = None;
        for dot in 0..TOTAL {
            let target = session.target();
            session.start_capture();
            let start = session.started.unwrap();
            let samples: Vec<_> = (0..50)
                .map(|i| Sample {
                    xy: [
                        (target[0] + 0.03 + ((i % 5) as f64 - 2.0) * 0.0003) as f32,
                        (target[1] - 0.02) as f32,
                    ],
                    valid: true,
                    timestamp_us: (dot * 100 + i) as i64,
                    received: start + SETTLE + Duration::from_millis(i as u64 * 30),
                })
                .collect();
            session.tick(&samples, start + SETTLE + COLLECT);
            assert_eq!(session.captures.len(), dot + 1);
            if dot == TRAIN_COUNT - 1 {
                fitted = Some(session.model.as_ref().unwrap().coefficients.clone());
            }
        }
        let report = session.finished.unwrap();
        assert_eq!(report.model.coefficients, fitted.unwrap());
        assert_eq!(report.training.len(), TRAIN_COUNT);
        assert_eq!(report.validation.len(), CHECK_COUNT);
        assert_eq!(report.metrics.corner_errors.len(), 4);
        assert!(report.metrics.recommend);
        assert!(report
            .validation
            .iter()
            .all(|v| report.training.iter().all(|t| t.target != v.target)));
        assert!(serde_json::to_string(&report).is_ok());
    }
    #[test]
    fn curvature_is_selected_when_it_generalizes_better_than_affine() {
        let mut session = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        for _ in 0..TRAIN_COUNT {
            let raw = session.target();
            let target = [
                raw[0] + 0.16 * (raw[1] - 0.5).powi(2),
                raw[1] + 0.12 * (raw[0] - 0.5).powi(2),
            ];
            session
                .captures
                .push(capture(target, [raw[0] - target[0], raw[1] - target[1]]));
        }
        assert_eq!(
            choose_model(&session.captures, [3840.0, 2160.0])
                .unwrap()
                .kind,
            "Quadratic"
        );
    }
    fn capture(target: XY, offset: XY) -> Capture {
        let observations = (0..50)
            .map(|i| Observation {
                xy: [
                    target[0] + offset[0] + ((i % 5) as f64 - 2.0) * 0.0004,
                    target[1] + offset[1] + ((i % 7) as f64 - 3.0) * 0.0003,
                ],
                valid: true,
                timestamp_us: i,
            })
            .collect();
        summarize(target, observations, [3840.0, 2160.0]).unwrap()
    }
    #[test]
    fn median_rejection_keeps_biased_cluster_and_rejects_far_outliers() {
        let mut cap = capture([0.5, 0.5], [0.04, -0.03]);
        for i in 50..55 {
            cap.observations.push(Observation {
                xy: [0.9, 0.1],
                valid: true,
                timestamp_us: i,
            });
        }
        let result = summarize(cap.target, cap.observations, [3840.0, 2160.0]).unwrap();
        assert_eq!(result.rejected.len(), 5);
        assert!((result.mean[0] - 0.54).abs() < 0.001);
    }
    #[test]
    fn sparse_or_drifting_capture_is_rejected() {
        assert!(summarize([0.5; 2], vec![], [3840.0, 2160.0]).is_err());
        let observations = (0..50)
            .map(|i| Observation {
                xy: [0.2 + i as f64 * 0.01, 0.5],
                valid: true,
                timestamp_us: i,
            })
            .collect();
        assert!(summarize([0.5; 2], observations, [3840.0, 2160.0]).is_err());
    }
    #[test]
    fn held_out_offset_is_improved_and_worsening_map_is_rejected() {
        let size = [3840.0, 2160.0];
        let mut session = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        for _ in 0..TRAIN_COUNT {
            session
                .captures
                .push(capture(session.target(), [0.03, -0.02]));
        }
        let model = choose_model(&session.captures, size).unwrap();
        assert_eq!(model.kind, "Affine");
        let validation = vec![
            capture([0.2, 0.34], [0.03, -0.02]),
            capture([0.8, 0.66], [0.03, -0.02]),
        ];
        let metrics = evaluate(&model, &validation, size);
        assert!(metrics.recommend);
        assert!(metrics.corrected_mean_px < 1.0);
        let changed = vec![
            capture([0.2, 0.34], [-0.03, 0.02]),
            capture([0.8, 0.66], [-0.03, 0.02]),
        ];
        assert!(!evaluate(&model, &changed, size).recommend);
    }
    #[test]
    fn recording_skips_settle_and_duplicate_samples() {
        let mut session = Session::new(Display {
            name: "test".into(),
            rect: [0, 0, 3840, 2160],
        });
        session.start_capture();
        let start = session.started.unwrap();
        let samples = vec![
            Sample {
                xy: [0.5, 0.5],
                valid: true,
                timestamp_us: 1,
                received: start,
            },
            Sample {
                xy: [0.5, 0.5],
                valid: true,
                timestamp_us: 2,
                received: start + SETTLE,
            },
        ];
        session.tick(&samples, start + SETTLE);
        session.tick(&samples, start + SETTLE);
        assert_eq!(session.observations.len(), 1);
        session.pause();
        assert!(!session.capturing());
    }
    #[test]
    fn identity_and_bounds_do_not_clamp_offscreen_gaze() {
        let model = Model {
            kind: "Affine".into(),
            coefficients: [vec![0.02, 0.0, 0.0], vec![0.0; 3]],
            training_cv_rms_px: 0.0,
            local: None,
        };
        assert_eq!(model.apply([1.2, 0.5]), [1.22, 0.5]);
        assert!(model.stable());
    }
}
