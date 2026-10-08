//! Frozen, opt-in whole-screen experiment. The private fit stays local and is
//! rejected on display/calibration mismatch. Never trains on a future click.
//! Context comparison allows JSON integer/float spelling and round-trip rounding.
use serde::{Deserialize, Serialize};
type XY = [f64; 2];
#[derive(Clone, Serialize, Deserialize)]
pub struct Model {
    pub version: u32,
    pub context: serde_json::Value,
    pub coefficients: Vec<XY>,
    pub training_samples: usize,
    pub trained_before_ms: u64,
}
impl Model {
    pub fn load(context: &str) -> Option<Self> {
        let m: Self =
            serde_json::from_slice(&std::fs::read("recordings/corner-model.json").ok()?).ok()?;
        m.matches(context).then_some(m)
    }
    fn matches(&self, context: &str) -> bool {
        self.version == 1
            && self.training_samples >= 30
            && self.coefficients.len() == 18
            && self
                .coefficients
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() < 1e6)
            && serde_json::from_str::<serde_json::Value>(context)
                .is_ok_and(|c| same_context(&c, &self.context))
    }
    pub fn offset(&self, base: XY, offset: XY, rect: [i32; 4]) -> XY {
        if !base.iter().chain(offset.iter()).all(|v| v.is_finite()) {
            return offset;
        }
        let size = [(rect[2] - rect[0]) as f64, (rect[3] - rect[1]) as f64];
        if size.iter().any(|&v| v <= 0.) {
            return offset;
        }
        let clamp = |p: XY| -> XY {
            std::array::from_fn(|i| p[i].clamp(rect[i] as f64, (rect[i + 2] - 1) as f64))
        };
        let live = clamp(std::array::from_fn(|i| base[i] + offset[i]));
        let [x, y] = std::array::from_fn(|i| (base[i] - rect[i] as f64) / size[i] - 0.5);
        if !(-0.75..=0.75).contains(&x) || !(-0.75..=0.75).contains(&y) {
            return offset;
        }
        let mut f = vec![1., x, y];
        for j in 0..3 {
            for i in 0..5 {
                f.push(
                    (-((x - (i as f64 / 4. - 0.5)).powi(2) + (y - (j as f64 / 2. - 0.5)).powi(2))
                        / (2. * 0.25f64.powi(2)))
                    .exp(),
                );
            }
        }
        let predicted = clamp(std::array::from_fn(|axis| {
            base[axis]
                + f.iter()
                    .zip(&self.coefficients)
                    .map(|(v, c)| v * c[axis])
                    .sum::<f64>()
        }));
        let delta: XY = std::array::from_fn(|i| predicted[i] - live[i]);
        let gain = 150. / delta[0].hypot(delta[1]).max(150.);
        std::array::from_fn(|i| live[i] + gain * delta[i] - base[i])
    }
}
fn same_context(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) else {
                return false;
            };
            (a - b).abs() <= 8. * f64::EPSILON * a.abs().max(b.abs()).max(1.)
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_context(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, a)| b.get(k).is_some_and(|b| same_context(a, b)))
        }
        _ => a == b,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> Model {
        let mut c = vec![[0., 0.]; 18];
        c[0] = [200., 200.];
        Model {
            version: 1,
            context: serde_json::json!({"test":1}),
            coefficients: c,
            training_samples: 40,
            trained_before_ms: 1,
        }
    }
    #[test]
    fn entire_display_changes_and_extra_motion_is_bounded() {
        let m = model();
        let r = [-1000, 0, 1000, 1000];
        for p in [[0., 500.], [900., 50.], [-950., 950.], [900., 950.]] {
            let delta = m.offset(p, [0., 0.], r);
            assert!(delta[0].hypot(delta[1]) > 0.);
            assert!(delta[0].hypot(delta[1]) <= 150.00001);
            for axis in 0..2 {
                assert!(
                    (r[axis] as f64..=(r[axis + 2] - 1) as f64).contains(&(p[axis] + delta[axis]))
                );
            }
        }
        let p = [-990., 10.];
        let delta = m.offset(p, [0., 0.], r);
        assert!((delta[0].hypot(delta[1]) - 150.).abs() < 1e-9);
        let base = [-1100., -100.];
        let delta = m.offset(base, [0., 0.], r);
        let landed = [base[0] + delta[0], base[1] + delta[1]];
        assert!(landed[0] >= -1000. && landed[1] >= 0.);
        assert!((landed[0] + 1000.).hypot(landed[1]) <= 150.00001);
    }
    #[test]
    fn mapping_is_continuous_across_former_boundary_and_invalid_models_fail_closed() {
        let mut m = model();
        assert!(m.matches("{\"test\":1}"));
        assert!(m.matches("{\"test\":1.0}"));
        assert!(same_context(
            &serde_json::json!({"a":[0,1.0]}),
            &serde_json::json!({"a":[0.0,1.0000000000000002]})
        ));
        assert!(!same_context(
            &serde_json::json!({"a":[0,1.0]}),
            &serde_json::json!({"a":[0,1.0001]})
        ));
        assert!(!m.matches("{\"test\":2}"));
        let at = m.offset([250., 20.], [0., 0.], [0, 0, 1000, 1000]);
        let d = m.offset([249.999, 20.], [0., 0.], [0, 0, 1000, 1000]);
        assert!((d[0] - at[0]).hypot(d[1] - at[1]) < 0.001);
        assert!(at[0].hypot(at[1]) > 100.);
        assert_eq!(
            m.offset([-1000., 20.], [0., 0.], [0, 0, 1000, 1000]),
            [0., 0.]
        );
        m.coefficients[0][0] = f64::NAN;
        assert!(!m.matches("{\"test\":1}"));
    }
}
