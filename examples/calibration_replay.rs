//! Refit saved captures without touching the tracker or overwriting recordings.
//! Selection uses training only; replayed checks are retrospective evidence.
#![allow(dead_code)]
#[path = "../src/calibration.rs"]
mod calibration;
mod tobii {
    #[derive(Clone, Copy)]
    pub struct Sample {
        pub xy: [f32; 2],
        pub valid: bool,
        pub timestamp_us: i64,
        pub received: std::time::Instant,
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<_> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        let report = calibration::Report::load_latest().ok_or("No valid saved calibration")?;
        println!("Restored latest: {} (schema {}), {} fitting / {} check points, automatic {}, mean {:.1}px",
            report.model.kind, report.schema_version, report.training.len(), report.validation.len(),
            report.metrics.recommend, report.metrics.corrected_mean_px);
        return Ok(());
    }
    for path in paths {
        let original: calibration::Report = serde_json::from_slice(&std::fs::read(&path)?)?;
        let refit = original.refit()?;
        if let Some(local) = calibration::local_model(&original.training, original.display.size()) {
            let check =
                calibration::evaluate(&local, &original.validation, original.display.size());
            println!("  Best stable local candidate: training CV {:.1}px, check mean {:.1}px, improved {}, automatic {}", local.training_cv_rms_px, check.corrected_mean_px, check.improved_targets, check.recommend);
        } else {
            println!("  No stable local candidate");
        }
        let m = &refit.metrics;
        println!("{path}: {} -> {}; training CV {:.1} -> {:.1}px; check mean {:.1} raw / {:.1} old / {:.1} new; RMS {:.1}; improved {}/{}; automatic {}",
            original.model.kind, refit.model.kind, original.model.training_cv_rms_px, refit.model.training_cv_rms_px,
            m.raw_mean_px, original.metrics.corrected_mean_px, m.corrected_mean_px,
            m.corrected_sample_rms_px, m.improved_targets, refit.validation.len(), m.recommend);
        for c in &m.corner_errors {
            println!("  {}: {:.1} -> {:.1}px", c.name, c.raw_px, c.corrected_px);
        }
    }
    Ok(())
}
