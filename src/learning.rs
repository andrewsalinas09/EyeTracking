//! Session-local implicit calibration. Clicks are noisy labels, not ground truth.
//! Store click minus the BASE gaze map, not minus the already corrected landing;
//! otherwise applying each residual repeatedly would create a feedback drift.
use std::collections::VecDeque;

type XY = [f64; 2];
const MAX_CLICK_MS: u32 = 1500;
const HISTORY_MS: u32 = 300_000;
const MAX_CORRECTION: f64 = 120.0;
const MAX_OFFSET: f64 = 80.0;
const CONSENSUS_RADIUS: f64 = 20.0;
const MAX_STEP: f64 = 2.0;

struct Attempt {
    time: u32,
    base: XY,
    landing: XY,
    last: XY,
    path: f64,
    surface: usize,
    rect: [i32; 4],
    down: Option<(u32, XY)>,
    drag_path: f64,
}
struct Label {
    time: u32,
    offset: XY,
}
pub struct Learner {
    pub enabled: bool,
    pub offset: XY,
    pub accepted: u64,
    pub updates: u64,
    pub rejected: u64,
    pub status: &'static str,
    pending: Option<Attempt>,
    labels: VecDeque<Label>,
}
impl Default for Learner {
    fn default() -> Self {
        Self {
            enabled: true,
            offset: [0.0; 2],
            accepted: 0,
            updates: 0,
            rejected: 0,
            status: "Waiting for jump + click",
            pending: None,
            labels: VecDeque::new(),
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
    pub fn reset(&mut self) {
        let enabled = self.enabled;
        *self = Self::default();
        self.enabled = enabled;
    }
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.cancel("Learning frozen");
    }
    pub fn cancel(&mut self, reason: &'static str) {
        if self.pending.take().is_some() {
            self.rejected += 1;
            self.status = reason;
        }
    }
    pub fn expire(&mut self, now: u32) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| now.wrapping_sub(p.time) > MAX_CLICK_MS)
        {
            self.cancel("Skipped: delayed click");
        }
    }
    pub fn begin(&mut self, now: u32, base: XY, landing: XY, surface: usize, rect: [i32; 4]) {
        self.cancel("Replaced by next jump");
        if !self.enabled || surface == 0 || !interior(base, rect) || !interior(landing, rect) {
            return;
        }
        self.pending = Some(Attempt {
            time: now,
            base,
            landing,
            last: landing,
            path: 0.0,
            surface,
            rect,
            down: None,
            drag_path: 0.0,
        });
        self.status = "Watching correction + click";
    }
    /// Flags are Win32 raw mouse left-down/up bits 1/2. All other buttons and
    /// scrolling invalidate the attempt. Commit only on release to reject drags.
    pub fn event(&mut self, now: u32, cursor: XY, flags: u16, surface: usize) {
        self.expire(now);
        let Some(p) = self.pending.as_mut() else {
            return;
        };
        if !self.enabled || flags & !3 != 0 || !interior(cursor, p.rect) || surface != p.surface {
            self.cancel("Skipped: scroll, other button or window");
            return;
        }
        let travel = distance(cursor, p.last);
        p.path += travel;
        p.last = cursor;
        if p.down.is_some() {
            p.drag_path += travel;
        }
        if p.path > 240.0 || distance(cursor, p.landing) > MAX_CORRECTION {
            self.cancel("Skipped: long correction");
            return;
        }
        if p.drag_path > 4.0 {
            self.cancel("Skipped: drag");
            return;
        }
        if flags & 1 != 0 {
            if p.down.is_some() || now.wrapping_sub(p.time) < 80 {
                self.cancel("Skipped: ambiguous click");
                return;
            }
            p.down = Some((now, cursor));
        }
        if flags & 2 != 0 {
            let Some((down_time, target)) = p.down else {
                self.cancel("Skipped: unmatched release");
                return;
            };
            if now.wrapping_sub(down_time) > 500 {
                self.cancel("Skipped: held click");
                return;
            }
            let label = [target[0] - p.base[0], target[1] - p.base[1]];
            self.pending = None;
            self.observe(now, label);
        }
    }
    fn observe(&mut self, now: u32, offset: XY) {
        self.labels
            .retain(|s| now.wrapping_sub(s.time) <= HISTORY_MS);
        self.labels.push_back(Label { time: now, offset });
        while self.labels.len() > 9 {
            self.labels.pop_front();
        }
        self.accepted += 1;
        if self.labels.len() < 5 {
            self.status = "Gathering 5 consistent clicks";
            return;
        }
        // Exponentially favor recent samples (one-minute half-life).
        let weight = |s: &Label| 2_f64.powf(-(now.wrapping_sub(s.time) as f64) / 60_000.0);
        let median: XY = std::array::from_fn(|axis| {
            let mut values: Vec<_> = self
                .labels
                .iter()
                .map(|s| (s.offset[axis], weight(s)))
                .collect();
            values.sort_by(|a, b| a.0.total_cmp(&b.0));
            let half = values.iter().map(|v| v.1).sum::<f64>() * 0.5;
            let mut sum = 0.0;
            for (value, w) in values {
                sum += w;
                if sum >= half {
                    return value;
                }
            }
            unreachable!()
        });
        let agreeing: Vec<_> = self
            .labels
            .iter()
            .filter(|s| distance(s.offset, median) <= CONSENSUS_RADIUS)
            .collect();
        let agreeing_weight = agreeing.iter().map(|s| weight(s)).sum::<f64>();
        if agreeing.len() < 5
            || agreeing_weight < self.labels.iter().map(weight).sum::<f64>() * 0.6
            || distance(offset, median) > CONSENSUS_RADIUS
        {
            self.status = "Waiting for consistent corrections";
            return;
        }
        let target = bounded(
            std::array::from_fn(|axis| {
                agreeing
                    .iter()
                    .map(|s| s.offset[axis] * weight(s))
                    .sum::<f64>()
                    / agreeing_weight
            }),
            MAX_OFFSET,
        );
        let step = bounded(
            std::array::from_fn(|axis| (target[axis] - self.offset[axis]) * 0.15),
            MAX_STEP,
        );
        self.offset = bounded(
            std::array::from_fn(|axis| self.offset[axis] + step[axis]),
            MAX_OFFSET,
        );
        if distance(step, [0.0; 2]) > 0.01 {
            self.updates += 1;
        }
        self.status = "Learning from consistent clicks";
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const RECT: [i32; 4] = [0, 0, 3840, 2160];
    fn click(l: &mut Learner, now: u32, error: XY) {
        let base = [1000.0, 1000.0];
        l.begin(
            now,
            base,
            [base[0] + l.offset[0], base[1] + l.offset[1]],
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
        for n in 0..100 {
            let old = l.offset;
            click(&mut l, n * 1000, [30.0, -20.0]);
            assert!(distance(old, l.offset) <= MAX_STEP + 1e-9);
            if n < 4 {
                assert_eq!(l.offset, [0.0; 2]);
            }
        }
        assert!(distance(l.offset, [30.0, -20.0]) < 0.1);
    }
    #[test]
    fn outlier_cannot_move_established_offset_and_recent_posture_changes_replace_old_labels() {
        let mut l = Learner::default();
        for n in 0..30 {
            click(&mut l, n * 1000, [20.0, 0.0]);
        }
        let old = l.offset;
        click(&mut l, 31_000, [-70.0, 60.0]);
        assert_eq!(l.offset, old);
        for n in 32..132 {
            click(&mut l, n * 1000, [-15.0, 10.0]);
        }
        assert!(distance(l.offset, [-15.0, 10.0]) < 0.1);
    }
    #[test]
    fn drag_long_move_delay_scroll_window_change_and_quick_click_are_rejected() {
        for kind in 0..6 {
            let mut l = Learner::default();
            l.begin(0, [1000.0; 2], [1000.0; 2], 1, RECT);
            match kind {
                0 => {
                    l.event(200, [1010.0; 2], 1, 1);
                    l.event(220, [1020.0; 2], 0, 1);
                }
                1 => l.event(100, [1200.0; 2], 0, 1),
                2 => l.event(1600, [1000.0; 2], 1, 1),
                3 => l.event(100, [1000.0; 2], 1024, 1),
                4 => l.event(100, [1000.0; 2], 1, 2),
                _ => l.event(10, [1000.0; 2], 1, 1),
            }
            l.event(250, [1010.0; 2], 2, 1);
            assert_eq!(l.accepted, 0, "case {kind}");
            assert_eq!(l.offset, [0.0; 2]);
        }
    }
    #[test]
    fn no_correction_clicks_counter_bias_and_reset_and_freeze_work() {
        let mut l = Learner::default();
        for n in 0..30 {
            click(&mut l, n * 1000, [20.0; 2]);
        }
        l.toggle();
        let offset = l.offset;
        click(&mut l, 31_000, [0.0; 2]);
        assert_eq!(l.offset, offset);
        l.toggle();
        for n in 32..132 {
            click(&mut l, n * 1000, [0.0; 2]);
        }
        assert!(distance(l.offset, [0.0; 2]) < 0.1);
        l.reset();
        assert_eq!(l.accepted, 0);
        assert_eq!(l.offset, [0.0; 2]);
    }
    #[test]
    fn clock_wrap_and_stale_history_and_offset_limit() {
        let mut l = Learner::default();
        click(&mut l, u32::MAX - 150, [30.0; 2]);
        assert_eq!(l.accepted, 1);
        for n in 0..100 {
            click(&mut l, 1000 + n * 1000, [75.0; 2]);
        }
        assert!(distance(l.offset, [0.0; 2]) <= MAX_OFFSET + 1e-9);
        let old = l.offset;
        click(&mut l, 500_000, [0.0; 2]);
        assert_eq!(l.offset, old);
    }
}
