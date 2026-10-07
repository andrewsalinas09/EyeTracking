//! SQLite persistence owned by one worker. Ordered transactions keep snapshots
//! and click evidence together; reset is acknowledged only after durable commit.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedLabel {
    pub timestamp_ms: u64,
    pub position: [f64; 2],
    pub offset: [f64; 2],
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedState {
    pub version: u32,
    pub cols: usize,
    pub rows: usize,
    pub offsets: Vec<[f64; 2]>,
    pub trained: Vec<bool>,
    pub labels: Vec<SavedLabel>,
    pub accepted: u64,
    pub updates: u64,
}
enum Message {
    Save(String, SavedState, SavedLabel),
    Load(String, mpsc::SyncSender<Result<Option<SavedState>, String>>),
    Reset(mpsc::SyncSender<Result<(), String>>),
}
pub struct Store {
    tx: Option<mpsc::Sender<Message>>,
    worker: Option<JoinHandle<()>>,
    status: Arc<Mutex<String>>,
}
struct Database(Connection);
impl Database {
    fn open(path: &Path) -> Result<Self, String> {
        let db = crate::capture::open_database(path)?;
        Ok(Self(db))
    }
    fn load(&self, key: &str) -> Result<Option<SavedState>, String> {
        let json: Option<String> = self
            .0
            .query_row(
                "SELECT state_json FROM profiles WHERE context=?1",
                [key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|e| format!("Invalid saved learning: {e}"))
        })
        .transpose()
    }
    fn save(&mut self, key: &str, state: &SavedState, label: &SavedLabel) -> Result<(), String> {
        let json = serde_json::to_string(state).map_err(|e| e.to_string())?;
        let timestamp = i64::try_from(label.timestamp_ms).map_err(|e| e.to_string())?;
        let tx = self.0.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO click_samples(context,timestamp_ms,x,y,dx,dy) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                key,
                timestamp,
                label.position[0],
                label.position[1],
                label.offset[0],
                label.offset[1]
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO profiles(context,state_json,updated_ms) VALUES(?1,?2,?3) ON CONFLICT(context) DO UPDATE SET state_json=excluded.state_json,updated_ms=excluded.updated_ms",params![key,json,timestamp]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    fn reset(&mut self) -> Result<(), String> {
        let tx = self.0.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM click_samples", [])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM profiles", [])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
}
impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let path = path.to_path_buf();
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let status = Arc::new(Mutex::new("SQLite ready".into()));
        let shared = status.clone();
        let worker = thread::spawn(move || {
            let mut db = match Database::open(&path) {
                Ok(db) => db,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            while let Ok(message) = rx.recv() {
                let result = match message {
                    Message::Save(key, state, label) => db.save(&key, &state, &label),
                    Message::Load(key, reply) => {
                        let result = db.load(&key);
                        let status = result.as_ref().map(|_| ()).map_err(Clone::clone);
                        let _ = reply.send(result);
                        status
                    }
                    Message::Reset(reply) => {
                        let result = db.reset();
                        let _ = reply.send(result.clone());
                        result
                    }
                };
                *shared.lock().unwrap() = match result {
                    Ok(()) => "Learning saved in SQLite".into(),
                    Err(e) => format!("Learning database error: {e}"),
                };
            }
        });
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Self {
            tx: Some(tx),
            worker: Some(worker),
            status,
        })
    }
    pub fn save(&self, key: String, state: SavedState, label: SavedLabel) {
        if self
            .tx
            .as_ref()
            .unwrap()
            .send(Message::Save(key, state, label))
            .is_err()
        {
            *self.status.lock().unwrap() = "Learning database writer stopped".into();
        }
    }
    pub fn load(&self, key: String) -> Result<Option<SavedState>, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .unwrap()
            .send(Message::Load(key, tx))
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
    pub fn reset(&self) -> Result<(), String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .unwrap()
            .send(Message::Reset(tx))
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_context_isolation_and_ordered_reset_are_durable() {
        let path = std::env::temp_dir().join(format!(
            "gaze-sqlite-{}-{}.sqlite3",
            std::process::id(),
            crate::learning_log::timestamp_ms()
        ));
        let state = SavedState {
            version: 1,
            cols: 1,
            rows: 1,
            offsets: vec![[12., -7.]],
            trained: vec![true],
            labels: vec![],
            accepted: 3,
            updates: 1,
        };
        let label = SavedLabel {
            timestamp_ms: 123,
            position: [0.5, 0.5],
            offset: [20., -10.],
        };
        {
            let store = Store::open(&path).unwrap();
            store.save("calibration-a".into(), state.clone(), label.clone());
            assert_eq!(
                store.load("calibration-a".into()).unwrap().unwrap().offsets,
                state.offsets
            );
            assert!(store.load("calibration-b".into()).unwrap().is_none());
        }
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(
                store
                    .load("calibration-a".into())
                    .unwrap()
                    .unwrap()
                    .accepted,
                3
            );
            store.save("calibration-b".into(), state, label);
            store.reset().unwrap(); // queued save must not resurrect after reset
            assert!(store.load("calibration-b".into()).unwrap().is_none());
        }
        {
            let db = Database::open(&path).unwrap();
            assert!(db.load("calibration-a").unwrap().is_none());
            let count: i64 =
                db.0.query_row("SELECT count(*) FROM click_samples", [], |r| r.get(0))
                    .unwrap();
            assert_eq!(count, 0);
        }
        std::fs::remove_file(path).unwrap();
    }
}
