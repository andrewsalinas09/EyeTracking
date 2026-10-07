//! Read-only, streaming export of the lossless archive. Each JSONL line includes
//! session identity; memory use is bounded by one compressed batch, not history.
use flate2::read::ZlibDecoder;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, BufWriter, Write};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    let path = value("--database").unwrap_or("recordings/learning.sqlite3");
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    if args.iter().any(|a| a == "--stats") {
        let scalar = |sql: &str| -> rusqlite::Result<i64> { db.query_row(sql, [], |r| r.get(0)) };
        let bytes = scalar("PRAGMA page_count")? * scalar("PRAGMA page_size")?;
        let clicks=scalar("SELECT count(*) FROM capture_index WHERE kind='click' AND (json_extract(data_json,'$.data.button_flags') & 1)!=0")?;
        let seconds=scalar("SELECT coalesce(sum(last_us-first_us),0)/1000000 FROM (SELECT session,min(first_us) first_us,max(last_us) last_us FROM capture_batches GROUP BY session)")?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"sqlite_logical_bytes":bytes,
                "compressed_stream_bytes":scalar("SELECT coalesce(sum(length(data)),0) FROM capture_batches")?,
                "uncompressed_stream_bytes":scalar("SELECT coalesce(sum(raw_bytes),0) FROM capture_batches")?,
                "recorded_events":scalar("SELECT coalesce(sum(event_count),0) FROM capture_batches")?,
                "left_button_down_events":clicks,"recorded_seconds":seconds,
                "capture_gap_reports":scalar("SELECT count(*) FROM capture_index WHERE kind='capture_gap'")?,
                "accepted_training_samples":scalar("SELECT count(*) FROM click_samples")?,
                "bytes_per_recorded_second":if seconds>0{Some(bytes as f64/seconds as f64)}else{None},
                "million_click_bytes_at_observed_activity":if clicks>0{Some(bytes as f64/clicks as f64*1e6)}else{None},
                "estimate_note":"Total disk cost depends on elapsed recording time and device rates as well as clicks. Small sessions include fixed setup/calibration overhead. WAL temporary space is additional."
            }))?
        );
        return Ok(());
    }
    let output = value("--output")
        .ok_or("Use --stats, or --output path.jsonl [--session ID] [--database path]")?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let mut out = BufWriter::new(file);
    let session = value("--session");
    let mut sessions=db.prepare("SELECT id,started_ms,ended_ms,metadata_json FROM capture_sessions WHERE (?1 IS NULL OR id=?1) ORDER BY started_ms")?;
    let rows = sessions.query_map([session], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (id, start, end, metadata) = row?;
        writeln!(
            out,
            "{}",
            json!({"kind":"session_metadata","session":id,"started_ms":start,"ended_ms":end,"data":serde_json::from_str::<Value>(&metadata)?})
        )?;
    }
    let mut batches=db.prepare("SELECT session,codec,data FROM capture_batches WHERE (?1 IS NULL OR session=?1) ORDER BY id")?;
    let mut rows = batches.query([session])?;
    while let Some(row) = rows.next()? {
        let session: String = row.get(0)?;
        let codec: String = row.get(1)?;
        let blob: Vec<u8> = row.get(2)?;
        if codec != "zlib-jsonl-v1" {
            return Err(format!("Unsupported archive codec {codec}").into());
        }
        for line in BufReader::new(ZlibDecoder::new(blob.as_slice())).lines() {
            let mut event: Value = serde_json::from_str(&line?)?;
            event["session"] = json!(session);
            serde_json::to_writer(&mut out, &event)?;
            out.write_all(b"\n")?;
        }
    }
    // Gaps are written directly to the index even when the producer queue was full.
    let mut gaps=db.prepare("SELECT session,data_json FROM capture_index WHERE kind='capture_gap' AND (?1 IS NULL OR session=?1)")?;
    for row in gaps.query_map([session], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (session, data) = row?;
        writeln!(
            out,
            "{}",
            json!({"kind":"capture_gap","session":session,"data":serde_json::from_str::<Value>(&data)?})
        )?;
    }
    out.flush()?;
    Ok(())
}
