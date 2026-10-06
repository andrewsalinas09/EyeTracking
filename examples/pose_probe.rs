//! Read-only capability probe for posture-aware calibration. It reports stream
//! availability and valid callback counts, never changes tracker calibration.
//! ABI: https://github.com/cmaybon/tobii-stream-engine/tree/master/tobii-stream-engine-sys/src
use libloading::Library;
use std::{
    ffi::{c_char, c_void, CStr, CString},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
type Handle = *mut c_void;
type Destroy = unsafe extern "C" fn(Handle) -> u32;
type Callback = unsafe extern "C" fn(*const c_void, *mut c_void);
type Subscribe = unsafe extern "C" fn(Handle, Callback, *mut c_void) -> u32;
#[repr(C)]
#[derive(Default)]
struct Version {
    major: i32,
    minor: i32,
    revision: i32,
    build: i32,
}
#[repr(C)]
struct Eyes {
    time: i64,
    left_valid: u32,
    left: [f32; 3],
    right_valid: u32,
    right: [f32; 3],
}
#[repr(C)]
struct Head {
    time: i64,
    position_valid: u32,
    position: [f32; 3],
    rotation_valid: [u32; 3],
    rotation: [f32; 3],
}
#[derive(Default)]
struct Counts {
    total: u64,
    position: u64,
    rotation: u64,
}
unsafe extern "C" fn eyes(data: *const c_void, user: *mut c_void) {
    let sample = &*data.cast::<Eyes>();
    let count = &mut *user.cast::<Counts>();
    count.total += 1;
    count.position += u64::from(
        (sample.left_valid == 1 && sample.left.iter().all(|v| v.is_finite()))
            || (sample.right_valid == 1 && sample.right.iter().all(|v| v.is_finite())),
    );
}
unsafe extern "C" fn head(data: *const c_void, user: *mut c_void) {
    let sample = &*data.cast::<Head>();
    let count = &mut *user.cast::<Counts>();
    count.total += 1;
    count.position +=
        u64::from(sample.position_valid == 1 && sample.position.iter().all(|v| v.is_finite()));
    count.rotation += u64::from(
        sample.rotation_valid.iter().all(|v| *v == 1)
            && sample.rotation.iter().all(|v| v.is_finite()),
    );
}
unsafe extern "C" fn url(value: *const c_char, user: *mut c_void) {
    (*user.cast::<Vec<CString>>()).push(CStr::from_ptr(value).to_owned());
}
struct Session {
    api: Handle,
    device: Handle,
    api_destroy: Destroy,
    device_destroy: Destroy,
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.device.is_null() {
                (self.device_destroy)(self.device);
            }
            if !self.api.is_null() {
                (self.api_destroy)(self.api);
            }
        }
    }
}
struct Subscription {
    name: &'static str,
    counts: Box<Counts>,
    device: Handle,
    unsubscribe: Destroy,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe {
            (self.unsubscribe)(self.device);
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let library = Library::new(r"C:\Program Files\Tobii\Tobii EyeX\tobii_stream_engine.dll")?;
        macro_rules! symbol {
            ($name:literal,$ty:ty) => {
                *library.get::<$ty>(concat!($name, "\0").as_bytes())?
            };
        }
        let message = symbol!(
            "tobii_error_message",
            unsafe extern "C" fn(u32) -> *const c_char
        );
        let check = |code: u32| -> Result<(), String> {
            if code == 0 {
                Ok(())
            } else {
                Err(CStr::from_ptr(message(code)).to_string_lossy().into_owned())
            }
        };
        let version_fn = symbol!(
            "tobii_get_api_version",
            unsafe extern "C" fn(*mut Version) -> u32
        );
        let mut version = Version::default();
        check(version_fn(&mut version))?;
        if version.major != 4 {
            return Err("This probe expects Stream Engine 4.x".into());
        }
        println!(
            "Stream Engine {}.{}.{}.{}",
            version.major, version.minor, version.revision, version.build
        );
        let create = symbol!(
            "tobii_api_create",
            unsafe extern "C" fn(*mut Handle, *const c_void, *const c_void) -> u32
        );
        let enumerate = symbol!(
            "tobii_enumerate_local_device_urls",
            unsafe extern "C" fn(
                Handle,
                unsafe extern "C" fn(*const c_char, *mut c_void),
                *mut c_void,
            ) -> u32
        );
        let device_create = symbol!(
            "tobii_device_create",
            unsafe extern "C" fn(Handle, *const c_char, u32, *mut Handle) -> u32
        );
        let supported = symbol!(
            "tobii_stream_supported",
            unsafe extern "C" fn(Handle, u32, *mut u32) -> u32
        );
        let process = symbol!("tobii_device_process_callbacks", Destroy);
        let mut session = Session {
            api: null_mut(),
            device: null_mut(),
            api_destroy: symbol!("tobii_api_destroy", Destroy),
            device_destroy: symbol!("tobii_device_destroy", Destroy),
        };
        check(create(&mut session.api, null(), null()))?;
        let mut urls = Vec::<CString>::new();
        check(enumerate(
            session.api,
            url,
            &mut urls as *mut _ as *mut c_void,
        ))?;
        let tracker = urls.first().ok_or("No tracker")?;
        check(device_create(
            session.api,
            tracker.as_ptr(),
            1,
            &mut session.device,
        ))?;
        let streams = [
            (1, "gaze_origin", eyes as Callback),
            (2, "eye_position_normalized", eyes as Callback),
            (4, "head_pose", head as Callback),
            (8, "user_position_guide", eyes as Callback),
        ];
        let mut active = Vec::new();
        for (id, name, callback) in streams {
            let mut available = 0;
            let code = supported(session.device, id, &mut available);
            println!("{name}: capability={available}, query={code}");
            if code != 0 || available == 0 {
                continue;
            }
            let subscribe =
                *library.get::<Subscribe>(format!("tobii_{name}_subscribe\0").as_bytes())?;
            let unsubscribe =
                *library.get::<Destroy>(format!("tobii_{name}_unsubscribe\0").as_bytes())?;
            let mut count = Box::new(Counts::default());
            let code = subscribe(
                session.device,
                callback,
                &mut *count as *mut Counts as *mut c_void,
            );
            println!(
                "{name}: subscribe={code} ({})",
                CStr::from_ptr(message(code)).to_string_lossy()
            );
            if code == 0 {
                active.push(Subscription {
                    name,
                    counts: count,
                    device: session.device,
                    unsubscribe,
                });
            }
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Err(error) = check(process(session.device)) {
                println!("Callback processing: {error}");
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        for subscription in active {
            let count = &subscription.counts;
            println!(
                "{}: {} callbacks, {} valid positions, {} valid rotations",
                subscription.name, count.total, count.position, count.rotation
            );
        }
        Ok(())
    }
}
