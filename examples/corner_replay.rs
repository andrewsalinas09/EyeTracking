//! Replay the exact shipped mapper against private held-out predictions.
//! cargo run --example corner_replay -- MODEL PREDICTIONS
#[allow(dead_code)]
#[path = "../src/corner.rs"]
mod corner;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3, "MODEL PREDICTIONS");
    let model: corner::Model = serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
    let rect: [i32; 4] = serde_json::from_value(model.context["display"]["rect"].clone()).unwrap();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
    for region in [
        "top_left",
        "top_right",
        "bottom_left",
        "bottom_right",
        "noncorner",
    ] {
        let mut old = Vec::new();
        let mut new = Vec::new();
        let mut worse50 = 0;
        for row in rows.iter().filter(|r| r["region"] == region) {
            let base: [f64; 2] = serde_json::from_value(row["base"].clone()).unwrap();
            let landing: [f64; 2] = serde_json::from_value(row["landing"].clone()).unwrap();
            let target: [f64; 2] = serde_json::from_value(row["target"].clone()).unwrap();
            let offset = model.offset(base, std::array::from_fn(|i| landing[i] - base[i]), rect);
            let pred: [f64; 2] = std::array::from_fn(|i| {
                (base[i] + offset[i]).clamp(rect[i] as f64, (rect[i + 2] - 1) as f64)
            });
            assert!((pred[0] - landing[0]).hypot(pred[1] - landing[1]) <= 150.00001);
            let before = (landing[0] - target[0]).hypot(landing[1] - target[1]);
            let after = (pred[0] - target[0]).hypot(pred[1] - target[1]);
            worse50 += usize::from(after > before + 50.);
            old.push(before);
            new.push(after);
        }
        old.sort_by(f64::total_cmp);
        new.sort_by(f64::total_cmp);
        if !old.is_empty() {
            let m = (old.len() - 1) / 2;
            let p = (old.len() - 1) * 9 / 10;
            println!(
                "{region}: n={} median {:.1}->{:.1}, p90 {:.1}->{:.1}, worse by >50px: {worse50}",
                old.len(),
                old[m],
                new[m],
                old[p],
                new[p]
            );
        }
    }
}
