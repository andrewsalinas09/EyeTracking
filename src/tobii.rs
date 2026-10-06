//! Minimal dynamically loaded Stream Engine 4.x ABI. Uses the installed runtime;
//! no vendor binaries, calibration writes, or USB driver replacement.
//! ABI reference: https://github.com/cmaybon/tobii-stream-engine
use libloading::Library;
use std::collections::VecDeque;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy)]
struct GazePoint {
    timestamp_us: i64,
    validity: u32,
    xy: [f32; 2],
}
#[repr(C)]
#[derive(Default)]
struct Version {
    major: i32,
    minor: i32,
    revision: i32,
    build: i32,
}
type Handle = *mut c_void;
type GazeCallback = unsafe extern "C" fn(*const GazePoint, *mut c_void);
type UrlCallback = unsafe extern "C" fn(*const c_char, *mut c_void);
type ApiCreate = unsafe extern "C" fn(*mut Handle, *const c_void, *const c_void) -> u32;
type Destroy = unsafe extern "C" fn(Handle) -> u32;
type Enumerate = unsafe extern "C" fn(Handle, UrlCallback, *mut c_void) -> u32;
type DeviceCreate = unsafe extern "C" fn(Handle, *const c_char, u32, *mut Handle) -> u32;
type Subscribe = unsafe extern "C" fn(Handle, GazeCallback, *mut c_void) -> u32;
type ErrorMessage = unsafe extern "C" fn(u32) -> *const c_char;

#[derive(Clone, Copy)]
pub struct Sample {
    pub xy: [f32; 2],
    pub valid: bool,
    pub timestamp_us: i64,
    pub received: Instant,
}
pub struct State {
    pub status: String,
    pub version: String,
    pub samples: VecDeque<Sample>,
    pub total: u64,
    pub valid_total: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            status: "Connecting to Tobii…".into(),
            version: String::new(),
            samples: VecDeque::new(),
            total: 0,
            valid_total: 0,
        }
    }
}
impl State {
    pub fn hz(&self) -> f64 {
        let now = Instant::now();
        let recent: Vec<_> = self
            .samples
            .iter()
            .filter(|s| now.duration_since(s.received) < Duration::from_secs(1))
            .collect();
        if recent.len() < 2 {
            return 0.0;
        }
        let dt = recent.last().unwrap().timestamp_us - recent[0].timestamp_us;
        if dt <= 0 {
            0.0
        } else {
            (recent.len() - 1) as f64 * 1_000_000.0 / dt as f64
        }
    }
}

struct Engine {
    _library: Library,
    api: Handle,
    device: Handle,
    subscribed: bool,
    api_destroy: Destroy,
    device_destroy: Destroy,
    unsubscribe: Destroy,
    process: Destroy,
    message: ErrorMessage,
}
impl Engine {
    // All API calls and callbacks stay on the owning worker thread.
    unsafe fn connect(shared: &Arc<Mutex<State>>) -> Result<Self, String> {
        let path = std::env::var_os("TOBII_STREAM_ENGINE_DLL")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(r"C:\Program Files\Tobii\Tobii EyeX\tobii_stream_engine.dll")
            });
        if !path.is_absolute() {
            return Err("TOBII_STREAM_ENGINE_DLL must be an absolute path".into());
        }
        let library =
            Library::new(&path).map_err(|e| format!("Cannot load {}: {e}", path.display()))?;
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {
                *library
                    .get::<$ty>(concat!($name, "\0").as_bytes())
                    .map_err(|e| e.to_string())?
            };
        }
        let version_fn = symbol!(
            "tobii_get_api_version",
            unsafe extern "C" fn(*mut Version) -> u32
        );
        let create = symbol!("tobii_api_create", ApiCreate);
        let enumerate = symbol!("tobii_enumerate_local_device_urls", Enumerate);
        let device_create = symbol!("tobii_device_create", DeviceCreate);
        let subscribe = symbol!("tobii_gaze_point_subscribe", Subscribe);
        let mut engine = Self {
            api_destroy: symbol!("tobii_api_destroy", Destroy),
            device_destroy: symbol!("tobii_device_destroy", Destroy),
            unsubscribe: symbol!("tobii_gaze_point_unsubscribe", Destroy),
            process: symbol!("tobii_device_process_callbacks", Destroy),
            message: symbol!("tobii_error_message", ErrorMessage),
            _library: library,
            api: null_mut(),
            device: null_mut(),
            subscribed: false,
        };
        let mut version = Version::default();
        engine.check(version_fn(&mut version), "Read API version")?;
        if version.major != 4 {
            return Err(format!(
                "Unsupported Stream Engine major version {}; expected 4",
                version.major
            ));
        }
        shared.lock().unwrap().version = format!(
            "{}.{}.{}.{}",
            version.major, version.minor, version.revision, version.build
        );
        let result = create(&mut engine.api, null(), null());
        engine.check(result, "Create API")?;
        let mut urls: Vec<CString> = Vec::new();
        engine.check(
            enumerate(engine.api, receive_url, &mut urls as *mut _ as *mut c_void),
            "Find tracker",
        )?;
        let url = urls
            .first()
            .ok_or("No Tobii tracker found. Check its USB connection and Tobii Experience.")?;
        let result = device_create(engine.api, url.as_ptr(), 1, &mut engine.device);
        engine.check(result, "Open tracker")?;
        // Arc allocation stays alive until after Engine is dropped/unsubscribed.
        engine.check(
            subscribe(
                engine.device,
                receive_gaze,
                Arc::as_ptr(shared) as *mut c_void,
            ),
            "Subscribe to gaze",
        )?;
        engine.subscribed = true;
        shared.lock().unwrap().status = "Connected".into();
        Ok(engine)
    }
    unsafe fn check(&self, code: u32, operation: &str) -> Result<(), String> {
        if code == 0 {
            return Ok(());
        }
        let ptr = (self.message)(code);
        let message = if ptr.is_null() {
            format!("error {code}")
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        };
        Err(format!("{operation}: {message} ({code})"))
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            if !self.device.is_null() {
                if self.subscribed {
                    (self.unsubscribe)(self.device);
                }
                (self.device_destroy)(self.device);
            }
            if !self.api.is_null() {
                (self.api_destroy)(self.api);
            }
        }
    }
}
unsafe extern "C" fn receive_url(url: *const c_char, context: *mut c_void) {
    if !url.is_null() && !context.is_null() {
        (*(context as *mut Vec<CString>)).push(CStr::from_ptr(url).to_owned());
    }
}
unsafe extern "C" fn receive_gaze(point: *const GazePoint, context: *mut c_void) {
    if point.is_null() || context.is_null() {
        return;
    }
    let p = *point;
    let shared = &*(context as *const Mutex<State>);
    // Never unwind across the C callback boundary.
    if let Ok(mut state) = shared.lock() {
        let sample = Sample {
            xy: p.xy,
            valid: p.validity == 1 && p.xy.iter().all(|x| x.is_finite()),
            timestamp_us: p.timestamp_us,
            received: Instant::now(),
        };
        state.total += 1;
        state.valid_total += u64::from(sample.valid);
        state.samples.push_back(sample);
        while state.samples.len() > 1024 {
            state.samples.pop_front();
        }
    }
}

pub struct Worker {
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = state.clone();
        let stopping = stop.clone();
        let join = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let result = unsafe { Engine::connect(&shared) };
                match result {
                    Ok(engine) => {
                        while !stopping.load(Ordering::Relaxed) {
                            let result = unsafe {
                                engine.check((engine.process)(engine.device), "Read gaze")
                            };
                            if let Err(error) = result {
                                shared.lock().unwrap().status = error;
                                break;
                            }
                            thread::sleep(Duration::from_millis(4));
                        }
                    }
                    Err(error) => shared.lock().unwrap().status = error,
                }
                // Retry without blocking shutdown for the entire retry interval.
                for _ in 0..20 {
                    if stopping.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self {
            state,
            stop,
            join: Some(join),
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
