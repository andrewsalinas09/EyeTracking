//! Versioned research archive, separate from eligibility and learned state.
//! Bounded, nonblocking producers; compressed batches and explicit loss counters
//! prevent high-rate devices or disk failures from silently corrupting a dataset.
use flate2::{write::ZlibEncoder, Compression};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, Weak,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const BYTE_BUDGET: usize = 8 * 1024 * 1024;
static ACTIVE: Mutex<Weak<Sink>> = Mutex::new(Weak::new());
static START_ERROR: Mutex<Option<String>> = Mutex::new(None);
static INTERACTION: AtomicU64 = AtomicU64::new(1);
pub fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn interaction_id() -> u64 {
    INTERACTION.fetch_add(1, Ordering::Relaxed)
}
pub unsafe fn input_device(handle: windows_sys::Win32::Foundation::HANDLE) {
    use windows_sys::Win32::UI::Input::*;
    let mut size = 0;
    let mut name = None;
    if !handle.is_null()
        && GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, std::ptr::null_mut(), &mut size)
            != u32::MAX
        && size < 32768
    {
        let mut chars = vec![0u16; size as usize + 1];
        if GetRawInputDeviceInfoW(
            handle,
            RIDI_DEVICENAME,
            chars.as_mut_ptr() as *mut _,
            &mut size,
        ) != u32::MAX
        {
            name = Some(String::from_utf16_lossy(
                &chars[..chars.iter().position(|&c| c == 0).unwrap_or(chars.len())],
            ));
        }
    }
    let mut info: RID_DEVICE_INFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<RID_DEVICE_INFO>() as u32;
    let mut size = info.cbSize;
    let details = if !handle.is_null()
        && GetRawInputDeviceInfoW(
            handle,
            RIDI_DEVICEINFO,
            &mut info as *mut _ as *mut _,
            &mut size,
        ) != u32::MAX
    {
        match info.dwType {
            RIM_TYPEMOUSE => {
                let m = info.Anonymous.mouse;
                json!({"type":"mouse","id":m.dwId,"buttons":m.dwNumberOfButtons,"sample_rate":m.dwSampleRate,"horizontal_wheel":m.fHasHorizontalWheel!=0})
            }
            RIM_TYPEHID => {
                let h = info.Anonymous.hid;
                json!({"type":"hid","vendor":h.dwVendorId,"product":h.dwProductId,"version":h.dwVersionNumber,"usage_page":h.usUsagePage,"usage":h.usUsage})
            }
            _ => Value::Null,
        }
    } else {
        Value::Null
    };
    record(
        "input_device",
        None,
        json!({"device_handle":handle as usize,"device_path":name,"details":details,"null_device":handle.is_null()}),
    );
}
pub fn record(kind: &str, interaction: Option<u64>, data: Value) {
    let sink = ACTIVE.lock().unwrap().upgrade();
    if let Some(sink) = sink {
        sink.record(kind, interaction, data);
    }
}
pub fn status() -> Option<String> {
    if let Some(e) = START_ERROR.lock().unwrap().as_ref() {
        return Some(format!("Research recording failed: {e}"));
    }
    ACTIVE.lock().unwrap().upgrade().map(|s| s.status())
}
pub fn open_database(path: &Path) -> Result<Connection, String> {
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.busy_timeout(Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version > 2 {
        return Err("Database belongs to a newer app".into());
    }
    if version == 1 {
        let backup = path.with_extension("v1-backup.sqlite3");
        if !backup.exists() {
            db.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
                .map_err(|e| format!("Before-migration backup failed: {e}"))?;
        }
    }
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS profiles(context TEXT PRIMARY KEY,state_json TEXT NOT NULL,updated_ms INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS click_samples(id INTEGER PRIMARY KEY,context TEXT NOT NULL,timestamp_ms INTEGER NOT NULL,x REAL NOT NULL,y REAL NOT NULL,dx REAL NOT NULL,dy REAL NOT NULL);
        CREATE INDEX IF NOT EXISTS samples_context ON click_samples(context,timestamp_ms);
        CREATE TABLE IF NOT EXISTS capture_sessions(id TEXT PRIMARY KEY,started_ms INTEGER NOT NULL,ended_ms INTEGER,metadata_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS capture_batches(id INTEGER PRIMARY KEY,session TEXT NOT NULL,first_seq INTEGER NOT NULL,last_seq INTEGER NOT NULL,first_us INTEGER NOT NULL,last_us INTEGER NOT NULL,event_count INTEGER NOT NULL,raw_bytes INTEGER NOT NULL,codec TEXT NOT NULL,data BLOB NOT NULL);
        CREATE INDEX IF NOT EXISTS capture_time ON capture_batches(session,first_us,last_us);
        CREATE TABLE IF NOT EXISTS capture_index(session TEXT NOT NULL,seq INTEGER NOT NULL,kind TEXT NOT NULL,interaction INTEGER,wall_ms INTEGER NOT NULL,mono_us INTEGER NOT NULL,data_json TEXT NOT NULL,PRIMARY KEY(session,seq));
        CREATE INDEX IF NOT EXISTS capture_interaction ON capture_index(session,interaction,seq);
        CREATE INDEX IF NOT EXISTS capture_kind ON capture_index(kind,wall_ms);
        PRAGMA user_version=2; COMMIT;").map_err(|e|e.to_string())?;
    Ok(db)
}
struct Event {
    seq: u64,
    mono: u64,
    wall: u64,
    kind: String,
    interaction: Option<u64>,
    bytes: Vec<u8>,
}
struct Sink {
    tx: mpsc::SyncSender<Event>,
    start: Instant,
    seq: AtomicU64,
    queued: Arc<AtomicUsize>,
    lost: Arc<AtomicU64>,
    saved: Arc<AtomicU64>,
    error: Arc<Mutex<Option<String>>>,
}
impl Sink {
    fn record(&self, kind: &str, interaction: Option<u64>, data: Value) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mono = self.start.elapsed().as_micros() as u64;
        let wall = timestamp_ms();
        let event = json!({"schema":1,"seq":seq,"mono_us":mono,"wall_ms":wall,
            "uptime_ms":unsafe{windows_sys::Win32::System::SystemInformation::GetTickCount64()},
            "kind":kind,"interaction":interaction,"data":data});
        let bytes = serde_json::to_vec(&event).expect("JSON Value serialization");
        let size = bytes.len();
        if self
            .queued
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n + size <= BYTE_BUDGET).then_some(n + size)
            })
            .is_err()
        {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self
            .tx
            .try_send(Event {
                seq,
                mono,
                wall,
                kind: kind.into(),
                interaction,
                bytes,
            })
            .is_err()
        {
            self.queued.fetch_sub(size, Ordering::Relaxed);
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn status(&self) -> String {
        let lost = self.lost.load(Ordering::Relaxed);
        if let Some(e) = self.error.lock().unwrap().as_ref() {
            return format!("Recording error: {e} ({lost} events lost)");
        }
        if lost > 0 {
            format!("Recording warning: {lost} events lost")
        } else {
            format!(
                "SQLite: {} research events saved",
                self.saved.load(Ordering::Relaxed)
            )
        }
    }
}
pub struct Archive {
    sink: Option<Arc<Sink>>,
    worker: Option<JoinHandle<()>>,
}
impl Archive {
    pub fn start(path: &Path, metadata: Value) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut db = open_database(path)?;
        let wall = timestamp_ms();
        let session = format!("{wall}-{}-{}", std::process::id(), interaction_id());
        db.execute(
            "INSERT INTO capture_sessions(id,started_ms,metadata_json) VALUES(?1,?2,?3)",
            params![session, wall as i64, metadata.to_string()],
        )
        .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::sync_channel::<Event>(8192);
        let queued = Arc::new(AtomicUsize::new(0));
        let lost = Arc::new(AtomicU64::new(0));
        let saved = Arc::new(AtomicU64::new(0));
        let error = Arc::new(Mutex::new(None));
        let sink = Arc::new(Sink {
            tx,
            start: Instant::now(),
            seq: AtomicU64::new(1),
            queued: queued.clone(),
            lost: lost.clone(),
            saved: saved.clone(),
            error: error.clone(),
        });
        let worker = thread::spawn(move || {
            let mut batch = Vec::new();
            let mut bytes = 0;
            let mut last = Instant::now();
            let mut reported_loss = 0;
            loop {
                let mut closed = false;
                let remaining = Duration::from_millis(250).saturating_sub(last.elapsed());
                match rx.recv_timeout(remaining) {
                    Ok(event) => {
                        bytes += event.bytes.len();
                        batch.push(event);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => closed = true,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if closed
                    || batch.len() >= 512
                    || bytes >= 512 * 1024
                    || last.elapsed() >= Duration::from_millis(250)
                {
                    let loss = lost.load(Ordering::Relaxed);
                    if !batch.is_empty() || loss != reported_loss {
                        match write_batch(&mut db, &session, &batch, loss, reported_loss) {
                            Ok(()) => {
                                saved.fetch_add(batch.len() as u64, Ordering::Relaxed);
                                reported_loss = loss;
                                *error.lock().unwrap() = None;
                            }
                            Err(e) => {
                                lost.fetch_add(batch.len() as u64, Ordering::Relaxed);
                                *error.lock().unwrap() = Some(e);
                            }
                        }
                    }
                    queued.fetch_sub(bytes, Ordering::Relaxed);
                    batch.clear();
                    bytes = 0;
                    last = Instant::now();
                }
                if closed {
                    break;
                }
            }
            let _ = db.execute(
                "UPDATE capture_sessions SET ended_ms=?2 WHERE id=?1",
                params![session, timestamp_ms() as i64],
            );
        });
        Ok(Self {
            sink: Some(sink),
            worker: Some(worker),
        })
    }
    pub fn install(&self) {
        *ACTIVE.lock().unwrap() = Arc::downgrade(self.sink.as_ref().unwrap());
        *START_ERROR.lock().unwrap() = None;
    }
    pub fn start_app() -> Option<Self> {
        match Self::start(
            Path::new("recordings/learning.sqlite3"),
            json!({
                "app_version":env!("CARGO_PKG_VERSION"),"source_fingerprint":env!("EYETRACKING_SOURCE"),
                "git_revision":env!("EYETRACKING_GIT"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
                "coordinates":"desktop physical pixels; raw gaze and position-guide coordinates normalized; head/origin mm; rotation radians",
                "clock":"mono_us is session elapsed Instant at enqueue; wall_ms is Unix time; uptime_ms is Windows boot clock; SDK and input-message timestamps remain in payload",
                "policy":"all available subscribed tracker samples and input events while app runs, including invalid/rejected/paused; no automatic retention deletion",
                "eye_images":"not captured; this integration exposes estimated gaze/pose, not eye-camera frames",
                "nonfinite":"numeric NaN/Infinity encoded as null; raw tracker float bits retained",
                "queue_byte_budget":BYTE_BUDGET,"batch_target_delay_ms":250,
            }),
        ) {
            Ok(a) => {
                a.install();
                record("session_start", None, json!({}));
                Some(a)
            }
            Err(e) => {
                *START_ERROR.lock().unwrap() = Some(e);
                None
            }
        }
    }
}
impl Drop for Archive {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.record(
                "session_end",
                None,
                json!({"lost_events":sink.lost.load(Ordering::Relaxed)}),
            );
            drop(sink);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn indexed(kind: &str) -> bool {
    !matches!(
        kind,
        "gaze"
            | "head_pose"
            | "gaze_origin"
            | "eye_position_normalized"
            | "user_position_guide"
            | "gaze_data"
            | "pointer"
            | "touchpad"
            | "raw_hid"
    )
}
fn write_batch(
    db: &mut Connection,
    session: &str,
    events: &[Event],
    lost: u64,
    previous: u64,
) -> Result<(), String> {
    let tx = db.transaction().map_err(|e| e.to_string())?;
    if !events.is_empty() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        let mut raw_bytes = 0;
        for e in events {
            encoder
                .write_all(&e.bytes)
                .and_then(|_| encoder.write_all(b"\n"))
                .map_err(|e| e.to_string())?;
            raw_bytes += e.bytes.len() + 1;
        }
        let compressed = encoder.finish().map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO capture_batches(session,first_seq,last_seq,first_us,last_us,event_count,raw_bytes,codec,data) VALUES(?1,?2,?3,?4,?5,?6,?7,'zlib-jsonl-v1',?8)",params![session,
            events.iter().map(|e|e.seq).min().unwrap() as i64,events.iter().map(|e|e.seq).max().unwrap() as i64,
            events.iter().map(|e|e.mono).min().unwrap() as i64,events.iter().map(|e|e.mono).max().unwrap() as i64,
            events.len() as i64,raw_bytes as i64,compressed]).map_err(|e|e.to_string())?;
        for e in events.iter().filter(|e| indexed(&e.kind)) {
            let mut value: Value = serde_json::from_slice(&e.bytes).map_err(|e| e.to_string())?;
            // Full maps are losslessly retained in the batch; keep the SQL index compact.
            if let Some(data) = value["data"].as_object_mut() {
                if data.remove("field").is_some() {
                    data.insert("field_in_batch".into(), json!(true));
                }
            }
            tx.execute("INSERT INTO capture_index(session,seq,kind,interaction,wall_ms,mono_us,data_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![session,e.seq as i64,e.kind,e.interaction.map(|n|n as i64),e.wall as i64,e.mono as i64,value.to_string()]).map_err(|e|e.to_string())?;
        }
    }
    if lost != previous {
        tx.execute("INSERT INTO capture_index(session,seq,kind,wall_ms,mono_us,data_json) VALUES(?1,?2,'capture_gap',?3,?4,?5)",params![session,-(lost as i64),timestamp_ms() as i64,events.last().map_or(0,|e|e.mono as i64),json!({"lost_total":lost,"lost_since_previous":lost-previous,"reason":"queue overflow or database write failure; events unavailable"}).to_string()]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    fn path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "gaze-archive-{label}-{}-{}.sqlite3",
            std::process::id(),
            interaction_id()
        ))
    }
    #[test]
    fn all_samples_roundtrip_with_timing_invalid_data_and_click_association() {
        let path = path("roundtrip");
        {
            let archive = Archive::start(&path, json!({"test":true})).unwrap();
            let sink = archive.sink.as_ref().unwrap();
            for n in 0..500 {
                sink.record("gaze",None,json!({"sdk_timestamp_us":n*1000,"validity":0,"xy":[null,-0.2],"xy_float_bits":[2143289344u32,3192704205u32]}));
            }
            sink.record(
                "jump",
                Some(14),
                json!({"base_unclamped":[100.,2300.],"actual_landing":[100.,2159.],"clamped":true}),
            );
            sink.record(
                "click",
                Some(14),
                json!({"cursor":[100.,2110.],"button_flags":1,"input_message_tick_ms":u32::MAX-4}),
            );
            sink.record(
                "attempt",
                Some(14),
                json!({"accepted":false,"status":"Skipped: edge landing","field":[[1.,2.]]}),
            );
        }
        let db = open_database(&path).unwrap();
        let count: i64 = db
            .query_row("SELECT sum(event_count) FROM capture_batches", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 504);
        let indexed: i64 = db
            .query_row(
                "SELECT count(*) FROM capture_index WHERE interaction=14",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 3);
        let mut stmt = db
            .prepare("SELECT data FROM capture_batches ORDER BY id")
            .unwrap();
        let mut events = Vec::new();
        for bytes in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).unwrap() {
            let mut text = String::new();
            flate2::read::ZlibDecoder::new(bytes.unwrap().as_slice())
                .read_to_string(&mut text)
                .unwrap();
            events.extend(
                text.lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap()),
            );
        }
        assert_eq!(events[0]["data"]["xy_float_bits"][0], 2143289344u32);
        assert!(events
            .iter()
            .all(|e| e["mono_us"].is_u64() && e["wall_ms"].is_u64() && e["uptime_ms"].is_u64()));
        assert_eq!(events[502]["data"]["field"], json!([[1., 2.]]));
        let summary: String = db
            .query_row(
                "SELECT data_json FROM capture_index WHERE kind='attempt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(summary.contains("field_in_batch"));
        drop(stmt);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn queue_overflow_is_counted_and_saved_as_an_explicit_gap() {
        let path = path("gap");
        {
            let archive = Archive::start(&path, json!({})).unwrap();
            let sink = archive.sink.as_ref().unwrap();
            sink.record("oversized", None, json!({"data":"x".repeat(BYTE_BUDGET+1)}));
            assert_eq!(sink.lost.load(Ordering::Relaxed), 1);
            assert!(sink.status().contains("1 events lost"));
            sink.record("click", Some(1), json!({"button_flags":1}));
        }
        let db = open_database(&path).unwrap();
        let gaps: i64 = db
            .query_row(
                "SELECT count(*) FROM capture_index WHERE kind='capture_gap'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gaps, 1);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn version_one_migration_keeps_existing_data_and_backup() {
        let path = path("migration");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE profiles(context TEXT PRIMARY KEY,state_json TEXT NOT NULL,updated_ms INTEGER NOT NULL); INSERT INTO profiles VALUES('test','{}',1); PRAGMA user_version=1;").unwrap();
        }
        let db = open_database(&path).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert!(path.with_extension("v1-backup.sqlite3").exists());
        drop(db);
        std::fs::remove_file(path.with_extension("v1-backup.sqlite3")).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
