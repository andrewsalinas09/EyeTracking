#![windows_subsystem = "windows"]
mod calibration;
mod gaze_dot;
mod learning;
mod learning_feedback;
mod learning_log;
mod learning_store;
mod mouse;
mod pose;
mod preferences;
mod preview;
mod scroll;
mod tobii;
mod touchpad;

fn main() {
    if let Err(error) = preferences::set_data_directory() {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let text: Vec<u16> = format!("EyeTracking could not open its data directory: {error}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let title: Vec<u16> = "EyeTracking".encode_utf16().chain(Some(0)).collect();
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_ICONERROR,
            );
        }
        return;
    }
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
