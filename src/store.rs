//! SQLite-backed event log. The database stores only the operation log; the
//! whole adjudication state is rebuilt by replaying it from the fixed fixture.

use crate::model::{apply, boot, Event, World};
use rusqlite::Connection;
use serde_json;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    event_json    TEXT NOT NULL,
    at_rfc3339    TEXT NOT NULL,
    note          TEXT
);
"#;

pub struct Store {
    pub conn: Connection,
}

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", 1)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    /// Fresh in-memory store (tests / ephemeral demos).
    pub fn memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn count_events(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
    }

    pub fn append(
        &mut self,
        event: &Event,
        at_rfc3339: &str,
        note: Option<&str>,
    ) -> rusqlite::Result<i64> {
        let json = serde_json::to_string(event).expect("event serializes");
        self.conn.execute(
            "INSERT INTO events(event_json, at_rfc3339, note) VALUES (?1, ?2, ?3)",
            rusqlite::params![json, at_rfc3339, note],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn load_log(&self) -> rusqlite::Result<Vec<(i64, Event, String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, event_json, at_rfc3339, note FROM events ORDER BY seq")?;
        let rows = stmt.query_map([], |r| {
            let seq: i64 = r.get(0)?;
            let json: String = r.get(1)?;
            let at: String = r.get(2)?;
            let note: Option<String> = r.get(3)?;
            Ok((seq, json, at, note))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, json, at, note) = row?;
            let event: Event = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            out.push((seq, event, at, note));
        }
        Ok(out)
    }

    pub fn clear(&mut self) -> rusqlite::Result<()> {
        self.conn
            .execute_batch("DELETE FROM events; DELETE FROM meta;")
    }

    /// Rebuild the world. A completely empty database is seeded with the
    /// default bootstrap event, which is persisted so every run replays it.
    pub fn rehydrate(&mut self) -> Result<World, String> {
        let mut log = self.load_log().map_err(|e| e.to_string())?;
        if log.is_empty() {
            let ev = Event::Bootstrap {
                calib_id: 1,
                mode: "amp".to_string(),
                threshold_amp: crate::fixture::DEFAULT_THRESHOLD_AMP,
                threshold_snr: crate::fixture::DEFAULT_THRESHOLD_SNR,
            };
            let now = now_rfc3339();
            let seq = self
                .append(&ev, &now, Some("default bootstrap"))
                .map_err(|e| e.to_string())?;
            log.push((seq, ev, now, Some("default bootstrap".to_string())));
        }

        let first = &log[0].1;
        let mut world = match first {
            Event::Bootstrap {
                calib_id,
                mode,
                threshold_amp,
                threshold_snr,
            } => boot(*calib_id, mode, *threshold_amp, *threshold_snr)?,
            _ => return Err("event log must begin with a bootstrap event".into()),
        };
        for (_seq, ev, _at, _note) in log.iter().skip(1) {
            apply(&mut world, ev)?;
        }
        Ok(world)
    }
}

pub fn now_rfc3339() -> String {
    // Audit-only wall-clock annotation (UTC, second precision). It is not part
    // of adjudication state and never affects replay results.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let ss = secs % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, h, m, ss
    )
}

/// Howard Hinnant's days -> civil date (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}
