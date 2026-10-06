//! Offline experimental models. Predictions never enter the mouse controller.
//! Every evaluation query must be predicted before its target is used to train.
use serde::{Deserialize, Serialize};
type XY = [f64; 2];
#[derive(Clone, Serialize, Deserialize)]
pub struct Datum {
    pub gaze: XY,
    pub pose: [f64; 6],
    pub error: XY,
}
fn scaled(p: [f64; 6]) -> [f64; 6] {
    std::array::from_fn(|i| p[i] / if i < 3 { 50.0 } else { 0.2 })
}
fn features(d: &Datum, origin: [f64; 6], pose: bool) -> Vec<f64> {
    let x = d.gaze[0] - 0.5;
    let y = d.gaze[1] - 0.5;
    let mut out = vec![1.0, x, y];
    if pose {
        let p = scaled(d.pose);
        for i in 0..6 {
            out.push(p[i] - origin[i]);
        }
    }
    out
}
fn fit_predict(data: &[Datum], query: &Datum, pose: bool) -> XY {
    if data.is_empty() {
        return [0.0; 2];
    }
    let origin = scaled(data[0].pose);
    let f = features(query, origin, pose);
    let n = f.len();
    let mut a = vec![vec![0.0; n + 2]; n];
    for (i, row) in a.iter_mut().enumerate() {
        row[i] = if i == 0 { 0.1 } else { 1.0 };
    }
    for d in data {
        let f = features(d, origin, pose);
        for i in 0..n {
            for j in 0..n {
                a[i][j] += f[i] * f[j];
            }
            for axis in 0..2 {
                a[i][n + axis] += f[i] * d.error[axis];
            }
        }
    }
    for i in 0..n {
        let pivot = (i..n)
            .max_by(|&j, &k| a[j][i].abs().total_cmp(&a[k][i].abs()))
            .unwrap();
        a.swap(i, pivot);
        let value = a[i][i];
        if value.abs() < 1e-10 {
            return [0.0; 2];
        }
        for cell in &mut a[i][i..n + 2] {
            *cell /= value;
        }
        let pivot_row = a[i].clone();
        for (k, row) in a.iter_mut().enumerate() {
            if k == i {
                continue;
            }
            let factor = row[i];
            for (cell, pivot) in row[i..n + 2].iter_mut().zip(&pivot_row[i..n + 2]) {
                *cell -= factor * pivot;
            }
        }
    }
    let result: XY = std::array::from_fn(|axis| (0..n).map(|i| f[i] * a[i][n + axis]).sum());
    let length = result[0].hypot(result[1]);
    result.map(|v| v * (80.0 / length.max(80.0)))
}
#[derive(Default)]
pub struct Models {
    data: Vec<Datum>,
    experts: Vec<Vec<Datum>>,
}
impl Models {
    pub fn predict(&self, d: &Datum) -> [XY; 3] {
        let shared = fit_predict(&self.data, d, false);
        let continuous = fit_predict(&self.data, d, true);
        let expert = self
            .closest(d)
            .filter(|(_, distance)| *distance < 2.0)
            .map(|(i, _)| fit_predict(&self.experts[i], d, false))
            .unwrap_or(shared);
        [shared, continuous, expert]
    }
    fn closest(&self, d: &Datum) -> Option<(usize, f64)> {
        let p = scaled(d.pose);
        self.experts
            .iter()
            .enumerate()
            .map(|(i, rows)| {
                let center: [f64; 6] = std::array::from_fn(|a| {
                    rows.iter().map(|r| scaled(r.pose)[a]).sum::<f64>() / rows.len() as f64
                });
                (
                    i,
                    (0..6)
                        .map(|a| (p[a] - center[a]).powi(2))
                        .sum::<f64>()
                        .sqrt(),
                )
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }
    pub fn train(&mut self, d: Datum) {
        let nearest = self.closest(&d);
        let i = match nearest {
            Some((i, distance)) if distance < 1.5 || self.experts.len() >= 6 => i,
            _ => {
                self.experts.push(Vec::new());
                self.experts.len() - 1
            }
        };
        self.experts[i].push(d.clone());
        self.data.push(d);
    }
    pub fn profiles(&self) -> usize {
        self.experts.len()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recalls_opposite_posture_errors_before_training_return_visit() {
        let mut m = Models::default();
        for mode in 0..2 {
            for i in 0..12 {
                m.train(Datum {
                    gaze: [(i % 4) as f64 / 3.0, (i / 4) as f64 / 2.0],
                    pose: [0.0, 0.0, mode as f64 * 150.0, 0.0, 0.0, 0.0],
                    error: if mode == 0 {
                        [30.0, -20.0]
                    } else {
                        [-30.0, 20.0]
                    },
                });
            }
        }
        assert_eq!(m.profiles(), 2);
        let query = Datum {
            gaze: [0.4, 0.6],
            pose: [0.0; 6],
            error: [0.0; 2],
        };
        let p = m.predict(&query);
        assert!((p[2][0] - 30.0).hypot(p[2][1] + 20.0) < 1.0);
        assert!(p[0][0].abs() < 1.0);
        let mut altered = query;
        altered.error = [999.0; 2];
        assert_eq!(m.predict(&altered), p);
    }
}
