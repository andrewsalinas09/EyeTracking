#![windows_subsystem = "windows"]
mod calibration;
mod gaze_dot;
mod learning;
mod learning_log;
mod mouse;
mod pose;
mod preview;
mod scroll;
mod tobii;
mod touchpad;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|s| s == "--probe") {
        let seconds = args
            .get(index + 1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8)
            .clamp(1, 60);
        let worker = tobii::Worker::start();
        for _ in 0..seconds {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let state = worker.state.lock().unwrap();
            println!(
                "{} | Stream Engine {} | samples {} valid {} | {:.1} Hz",
                state.status,
                state.version,
                state.total,
                state.valid_total,
                state.hz()
            );
        }
        let valid = worker.state.lock().unwrap().valid_total;
        drop(worker);
        if valid == 0 {
            std::process::exit(1);
        }
    } else {
        preview::run();
    }
}
