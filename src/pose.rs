//! Pose evidence is joined by device timestamp, never by callback arrival order.
//! Match the nearest sample including invalid entries, so a blink cannot revive
//! an older valid posture. Missing/stale pose stays missing rather than zero.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
pub struct Evidence {
    pub gaze_timestamp_us: i64,
    pub raw_gaze: [f64; 2],
    pub head: Option<Head>,
    pub eye_origin: Option<Eyes>,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
pub struct Head {
    pub timestamp_us: i64,
    pub skew_us: i64,
    pub position: [f64; 3],
    pub rotation: [f64; 3],
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
pub struct Eyes {
    pub timestamp_us: i64,
    pub skew_us: i64,
    pub left: Option<[f64; 3]>,
    pub right: Option<[f64; 3]>,
}
pub struct Timed<T> {
    pub timestamp_us: i64,
    pub received: Instant,
    pub value: Option<T>,
}
pub fn nearest<T: Copy>(samples: &VecDeque<Timed<T>>, gaze_time: i64, now: Instant) -> Option<T> {
    let sample = samples
        .iter()
        .min_by_key(|s| s.timestamp_us.abs_diff(gaze_time))?;
    if sample.timestamp_us.abs_diff(gaze_time) > 50_000
        || now.saturating_duration_since(sample.received) > Duration::from_millis(200)
    {
        return None;
    }
    sample.value
}
pub fn push<T>(samples: &mut VecDeque<Timed<T>>, timestamp_us: i64, value: Option<T>) {
    samples.push_back(Timed {
        timestamp_us,
        received: Instant::now(),
        value,
    });
    while samples.len() > 128 {
        samples.pop_front();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_join_rejects_stale_invalid_and_distant_pose() {
        let now = Instant::now();
        let mut frames = VecDeque::new();
        frames.push_back(Timed {
            timestamp_us: 100_000,
            received: now,
            value: Some(1),
        });
        frames.push_back(Timed {
            timestamp_us: 130_000,
            received: now,
            value: None,
        });
        assert_eq!(nearest(&frames, 100_001, now), Some(1));
        assert_eq!(nearest(&frames, 130_001, now), None);
        assert_eq!(nearest(&frames, 300_000, now), None);
        assert_eq!(
            nearest(&frames, 100_000, now + Duration::from_millis(201)),
            None
        );
    }
}
