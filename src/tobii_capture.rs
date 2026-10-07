//! Optional read-only Stream Engine 4.x streams for future replay. ABI mirrors
//! cmaybon/tobii-stream-engine-sys/{streams,advanced,core}.rs. Availability and
//! subscription errors are recorded; unsupported/licensed streams stay absent.
use libloading::Library;
use serde::Serialize;
use serde_json::json;
use std::{
    ffi::{c_char, c_void},
    sync::atomic::{AtomicU64, Ordering},
};
type Destroy = unsafe extern "C" fn(*mut c_void) -> u32;
static CONNECTION: AtomicU64 = AtomicU64::new(0);
pub fn connected_attempt() {
    let id = CONNECTION.fetch_add(1, Ordering::Relaxed) + 1;
    crate::capture::record(
        "tracker_connection",
        None,
        json!({"connection":id,"state":"connecting"}),
    );
}
pub fn record(kind: &str, mut data: serde_json::Value) {
    data["connection"] = json!(CONNECTION.load(Ordering::Relaxed));
    crate::capture::record(kind, None, data);
}
#[repr(C)]
#[derive(Clone, Copy, Serialize)]
struct Eye {
    origin_validity: u32,
    origin_mm: [f32; 3],
    position_validity: u32,
    position_normalized: [f32; 3],
    gaze_validity: u32,
    gaze_mm: [f32; 3],
    gaze_normalized: [f32; 2],
    eyeball_validity: u32,
    eyeball_mm: [f32; 3],
    pupil_validity: u32,
    pupil_mm: f32,
}
#[repr(C)]
struct GazeData {
    tracker_us: i64,
    system_us: i64,
    left: Eye,
    right: Eye,
}
#[repr(C)]
struct Eyes {
    timestamp_us: i64,
    left_validity: u32,
    left: [f32; 3],
    right_validity: u32,
    right: [f32; 3],
}
#[repr(C)]
struct DeviceInfo {
    serial: [c_char; 256],
    model: [c_char; 256],
    generation: [c_char; 256],
    firmware: [c_char; 256],
    integration: [c_char; 128],
    hw_calibration: [c_char; 128],
    hw_date: [c_char; 128],
    lot: [c_char; 128],
    integration_type: [c_char; 256],
    runtime: [c_char; 256],
}
fn string(v: &[c_char]) -> String {
    String::from_utf8_lossy(
        &v.iter()
            .take_while(|&&x| x != 0)
            .map(|&x| x as u8)
            .collect::<Vec<_>>(),
    )
    .into_owned()
}
unsafe extern "C" fn gaze(data: *const c_void, _: *mut c_void) {
    if data.is_null() {
        return;
    }
    let p = &*data.cast::<GazeData>();
    let bits = |e: Eye| json!({"origin":e.origin_mm.map(f32::to_bits),"position":e.position_normalized.map(f32::to_bits),"gaze_mm":e.gaze_mm.map(f32::to_bits),"gaze_normalized":e.gaze_normalized.map(f32::to_bits),"eyeball":e.eyeball_mm.map(f32::to_bits),"pupil":e.pupil_mm.to_bits()});
    record(
        "gaze_data",
        json!({"timestamp_tracker_us":p.tracker_us,"timestamp_system_us":p.system_us,"left":p.left,"right":p.right,"left_float_bits":bits(p.left),"right_float_bits":bits(p.right)}),
    );
}
unsafe fn eyes(data: *const c_void, kind: &str) {
    if data.is_null() {
        return;
    }
    let p = &*data.cast::<Eyes>();
    record(
        kind,
        json!({"sdk_timestamp_us":p.timestamp_us,"left_validity":p.left_validity,"right_validity":p.right_validity,"left":p.left,"right":p.right,"left_float_bits":p.left.map(f32::to_bits),"right_float_bits":p.right.map(f32::to_bits)}),
    );
}
unsafe extern "C" fn position(data: *const c_void, _: *mut c_void) {
    eyes(data, "eye_position_normalized");
}
unsafe extern "C" fn guide(data: *const c_void, _: *mut c_void) {
    eyes(data, "user_position_guide");
}
unsafe extern "C" fn presence(status: u32, timestamp_us: i64, _: *mut c_void) {
    record(
        "user_presence",
        json!({"status":status,"sdk_timestamp_us":timestamp_us}),
    );
}
pub unsafe fn subscribe(
    library: &Library,
    device: *mut c_void,
    context: *mut c_void,
) -> Vec<Destroy> {
    let mut active = Vec::new();
    type Callback = unsafe extern "C" fn(*const c_void, *mut c_void);
    type Subscribe = unsafe extern "C" fn(*mut c_void, Callback, *mut c_void) -> u32;
    for (name, callback) in [
        ("gaze_data", gaze as Callback),
        ("eye_position_normalized", position as Callback),
        ("user_position_guide", guide as Callback),
    ] {
        let sub = library.get::<Subscribe>(format!("tobii_{name}_subscribe\0").as_bytes());
        let unsub = library.get::<Destroy>(format!("tobii_{name}_unsubscribe\0").as_bytes());
        let result = match (sub, unsub) {
            (Ok(sub), Ok(unsub)) => {
                let code = sub(device, callback, context);
                if code == 0 {
                    active.push(*unsub);
                }
                Some(code)
            }
            _ => None,
        };
        record(
            "stream_availability",
            json!({"stream":name,"subscribe_code":result,"available":result==Some(0),"missing_symbol":result.is_none()}),
        );
    }
    type Presence = unsafe extern "C" fn(
        *mut c_void,
        unsafe extern "C" fn(u32, i64, *mut c_void),
        *mut c_void,
    ) -> u32;
    if let (Ok(sub), Ok(unsub)) = (
        library.get::<Presence>(b"tobii_user_presence_subscribe\0"),
        library.get::<Destroy>(b"tobii_user_presence_unsubscribe\0"),
    ) {
        let code = sub(device, presence, context);
        if code == 0 {
            active.push(*unsub);
        }
        record(
            "stream_availability",
            json!({"stream":"user_presence","subscribe_code":code,"available":code==0}),
        );
    } else {
        record(
            "stream_availability",
            json!({"stream":"user_presence","available":false,"missing_symbol":true}),
        );
    }
    type Info = unsafe extern "C" fn(*mut c_void, *mut DeviceInfo) -> u32;
    if let Ok(info) = library.get::<Info>(b"tobii_get_device_info\0") {
        let mut d: DeviceInfo = std::mem::zeroed();
        let code = info(device, &mut d);
        record(
            "tracker_device",
            if code == 0 {
                json!({"result":code,"serial":string(&d.serial),"model":string(&d.model),"generation":string(&d.generation),"firmware":string(&d.firmware),"integration":string(&d.integration),"hw_calibration":string(&d.hw_calibration),"hw_date":string(&d.hw_date),"lot":string(&d.lot),"integration_type":string(&d.integration_type),"runtime":string(&d.runtime)})
            } else {
                json!({"result":code})
            },
        );
    }
    active
}
