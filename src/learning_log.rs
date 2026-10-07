//! Append-only learning evidence. Disk writes and report generation run off the
//! input thread, so saving a click cannot stall a pointer jump or a scroll hook.
//! Build HTML by streaming the journal: retaining every dense field in a Vec
//! made RAM grow with the user's lifetime click history.
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

enum Message {
    Record(Value),
    Open,
}
pub struct Recorder {
    tx: Option<mpsc::Sender<Message>>,
    worker: Option<JoinHandle<()>>,
    status: Arc<Mutex<String>>,
}
pub fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
impl Recorder {
    pub fn start(directory: &Path) -> Result<Self, String> {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = directory.join(format!("learning-{stamp}-{}.jsonl", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new("Recording learning data".into()));
        let result = status.clone();
        let worker = thread::spawn(move || run(file, path, rx, result));
        Ok(Self {
            tx: Some(tx),
            worker: Some(worker),
            status,
        })
    }
    pub fn record(&self, mut value: Value) {
        value["timestamp_ms"] = json!(timestamp_ms());
        if self
            .tx
            .as_ref()
            .unwrap()
            .send(Message::Record(value))
            .is_err()
        {
            *self.status.lock().unwrap() = "Learning log writer stopped".into();
        }
    }
    pub fn open(&self) -> Result<(), String> {
        self.tx
            .as_ref()
            .unwrap()
            .send(Message::Open)
            .map_err(|e| e.to_string())
    }
    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn run(file: File, path: PathBuf, rx: mpsc::Receiver<Message>, status: Arc<Mutex<String>>) {
    let mut writer = BufWriter::new(file);
    let mut saved = 0;
    let mut save_failed = false;
    let report = path.with_extension("html");
    while let Ok(message) = rx.recv() {
        match message {
            Message::Record(record) => {
                // A partial line after an I/O failure must not be followed by
                // another record on the same line. Report failure explicitly;
                // the independent SQLite archive still receives these events.
                let result = if save_failed {
                    Err("writer stopped after an earlier save failure".into())
                } else {
                    append(&mut writer, &record)
                };
                match result {
                    Ok(()) => {
                        saved += 1;
                        if !save_failed {
                            *status.lock().unwrap() =
                                format!("Saved {saved} learning events | L: map");
                        }
                    }
                    Err(e) => {
                        save_failed = true;
                        *status.lock().unwrap() = format!("Learning save failed: {e}");
                    }
                }
            }
            Message::Open => {
                let result = write_report(&report, &path).and_then(|()| {
                    std::process::Command::new("explorer.exe")
                        .arg(&report)
                        .spawn()
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
                if let Err(e) = result {
                    *status.lock().unwrap() = format!("Learning map failed: {e}");
                }
            }
        }
    }
    // Keep a directly openable snapshot even when the user never presses L.
    if let Err(e) = write_report(&report, &path) {
        *status.lock().unwrap() = format!("Learning map save failed: {e}");
    }
}
fn append(writer: &mut impl Write, record: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, record).map_err(|e| e.to_string())?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|e| e.to_string())
}
fn write_report(path: &Path, journal: &Path) -> Result<(), String> {
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let (prefix, suffix) = include_str!("learning_view.html")
            .split_once("/*RECORDS*/[]")
            .ok_or("Missing report marker")?;
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(prefix.as_bytes())?;
        out.write_all(b"[")?;
        let mut first = true;
        for line in BufReader::new(File::open(journal)?).lines() {
            let line = line?;
            // A truncated tail from an interrupted write is not valid evidence.
            if serde_json::from_str::<Value>(&line).is_err() {
                break;
            }
            if !first {
                out.write_all(b",")?;
            }
            first = false;
            out.write_all(line.replace('<', "\\u003c").as_bytes())?;
        }
        out.write_all(b"]")?;
        out.write_all(suffix.as_bytes())?;
        out.flush()?;
        Ok(())
    };
    run().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_flushes_on_drop_and_report_safely_embeds_saved_events() {
        let directory = std::env::temp_dir().join(format!(
            "gaze-log-test-{}-{}",
            std::process::id(),
            timestamp_ms()
        ));
        let log = Recorder::start(&directory).unwrap();
        log.record(json!({"kind":"attempt", "target":[30,40], "status":"</script>"}));
        log.record(json!({"kind":"reset", "epoch":1}));
        drop(log);
        let files: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let path = files
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .unwrap();
        let data = fs::read_to_string(path).unwrap();
        let records: Vec<Value> = data
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["target"], json!([30, 40]));
        assert!(records[0]["timestamp_ms"].as_u64().unwrap() > 0);
        let html = fs::read_to_string(path.with_extension("html")).unwrap();
        assert!(html.contains("\\u003c/script>"));
        assert!(!html.contains("/*RECORDS*/[]"));
        for file in files {
            fs::remove_file(file).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }
}
