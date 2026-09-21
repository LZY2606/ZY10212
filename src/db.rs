//! SQLite storage: append-only event log, import/reset/replay and state reads.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::sync::Mutex;

use crate::err::{bad, AppError};
use crate::fixture::Fixture;
use crate::signal::{analyze, envelope, AnalConfig, Candidate, Gate, Split, ThresholdMode};

pub struct Db(pub Mutex<Connection>);

#[derive(Debug, Clone)]
pub struct ConfigPatch {
    pub velocity_m_s: Option<f64>,
    pub probe_delay_ns: Option<f64>,
    pub threshold_mode: Option<String>,
    pub amp_threshold: Option<f64>,
    pub snr_threshold: Option<f64>,
    pub min_resolvable_ns: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct GatePatch {
    pub start: usize,
    pub end: usize,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    type TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS positions (
    idx INTEGER PRIMARY KEY,
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    x_mm REAL NOT NULL,
    y_mm REAL NOT NULL,
    wave TEXT NOT NULL,
    surface_sample INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS config (id INTEGER PRIMARY KEY CHECK (id = 0), v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS gates (id TEXT PRIMARY KEY, label TEXT NOT NULL, start_i INTEGER NOT NULL, end_i INTEGER NOT NULL, role TEXT NOT NULL, ord INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS decisions (
    position_idx INTEGER NOT NULL,
    candidate_key TEXT NOT NULL,
    verdict TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    candidate_snapshot TEXT NOT NULL,
    PRIMARY KEY (position_idx, candidate_key)
);
CREATE TABLE IF NOT EXISTS splits (
    position_idx INTEGER NOT NULL,
    parent_key TEXT NOT NULL,
    cuts TEXT NOT NULL,
    PRIMARY KEY (position_idx, parent_key)
);
CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    created_seq INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS track_members (
    track_id INTEGER NOT NULL,
    position_idx INTEGER NOT NULL,
    candidate_key TEXT NOT NULL,
    ord INTEGER NOT NULL,
    PRIMARY KEY (position_idx, candidate_key)
);
"#;

impl Db {
    pub fn file(path: &str) -> Result<Self, AppError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(mut conn: Connection) -> Result<Self, AppError> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        let has_fixture: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv WHERE k='fixture')",
            [],
            |r| r.get(0),
        )?;
        if !has_fixture {
            // Fresh database: bootstrap the bundled fixed fixture and record it.
            let fx_json = crate::FIXTURE_JSON;
            apply_fixture(&mut conn, fx_json)?;
            log_event(&mut conn, "fixture_imported", &json!({"fixture": fx_json}))?;
        }
        Ok(Db(Mutex::new(conn)))
    }

    pub fn cmd(&self, etype: &str, payload: &Value) -> Result<i64, AppError> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        apply_event(&tx, etype, payload)?;
        let seq = log_event(&tx, etype, payload)?;
        tx.commit()?;
        Ok(seq)
    }

    /// Wipe everything and re-apply an exported event list (first event must be the import).
    pub fn replay_export(&self, events: &[Value]) -> Result<usize, AppError> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute_batch(
            "DELETE FROM track_members; DELETE FROM tracks; DELETE FROM splits;
             DELETE FROM decisions; DELETE FROM gates; DELETE FROM config;
             DELETE FROM positions; DELETE FROM kv; DELETE FROM events;",
        )?;
        for ev in events {
            let etype = ev
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError("event missing type".to_string()))?;
            let payload = ev.get("payload").cloned().unwrap_or(json!({}));
            apply_event(&tx, etype, &payload)?;
            log_event(&tx, etype, &payload)?;
        }
        tx.commit()?;
        Ok(events.len())
    }

    pub fn reset_to_bundled(&self) -> Result<usize, AppError> {
        self.replay_export(&[json!({"type": "fixture_imported",
            "payload": {"fixture": crate::FIXTURE_JSON}})])
    }

    pub fn export_events(&self) -> Result<Vec<Value>, AppError> {
        let c = self.0.lock().unwrap();
        let mut stmt = c.prepare("SELECT seq, ts, type, payload FROM events ORDER BY seq")?;
        let rows = stmt.query_map([], |r| {
            let payload: String = r.get(3)?;
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "ts": r.get::<_, String>(1)?,
                "type": r.get::<_, String>(2)?,
                "payload": serde_json::from_str::<Value>(&payload).unwrap_or(json!({}))
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn state(&self) -> Result<Value, AppError> {
        let c = self.0.lock().unwrap();
        let fixture_json: Option<String> = c
            .query_row("SELECT v FROM kv WHERE k='fixture'", [], |r| r.get(0))
            .ok();
        let fixture_json = match fixture_json {
            Some(j) => j,
            None => {
                return Ok(json!({"fixture": null, "config": null, "gates": [],
                    "positions": [], "tracks": []}));
            }
        };
        let fx = Fixture::parse(&fixture_json).map_err(AppError)?;
        let cfg_json: String = c.query_row("SELECT v FROM config WHERE id=0", [], |r| r.get(0))?;
        let cfg: AnalConfig = serde_json::from_str(&cfg_json)?;

        let mut gates: Vec<Value> = Vec::new();
        {
            let mut stmt =
                c.prepare("SELECT id,label,start_i,end_i,role FROM gates ORDER BY ord")?;
            let rows = stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "label": r.get::<_, String>(1)?,
                    "start": r.get::<_, i64>(2)?,
                    "end": r.get::<_, i64>(3)?,
                    "role": r.get::<_, String>(4)?
                }))
            })?;
            for r in rows {
                gates.push(r?);
            }
        }

        let gate_list: Vec<Gate> = gates
            .iter()
            .map(|g| Gate {
                id: g["id"].as_str().unwrap().to_string(),
                label: g["label"].as_str().unwrap().to_string(),
                start: g["start"].as_u64().unwrap() as usize,
                end: g["end"].as_u64().unwrap() as usize,
                role: g["role"].as_str().unwrap().to_string(),
            })
            .collect();

        let mut positions = Vec::new();
        {
            let mut stmt = c.prepare(
                "SELECT idx,id,name,x_mm,y_mm,wave,surface_sample FROM positions ORDER BY idx",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })?;
            for row in rows {
                let (idx, id, name, x, y, wave_json, surf) = row?;
                let wave: Vec<f64> = serde_json::from_str(&wave_json)?;
                let splits = load_splits(&c, idx as usize)?;
                let mut analysis = analyze(&wave, &gate_list, &cfg, surf as usize, &splits);
                attach_decisions(&mut analysis.candidates, &c, idx as usize)?;
                let env = envelope(&wave);
                positions.push(json!({
                    "index": idx,
                    "id": id,
                    "name": name,
                    "x_mm": x,
                    "y_mm": y,
                    "surface_sample": surf,
                    "wave": wave,
                    "envelope": env,
                    "analysis": analysis,
                    "splits": splits
                }));
            }
        }

        let tracks = load_tracks(&c)?;

        Ok(json!({
            "fixture": fx,
            "config": cfg,
            "gates": gates,
            "positions": positions,
            "tracks": tracks
        }))
    }
}

fn load_splits(conn: &Connection, idx: usize) -> Result<Vec<Split>, AppError> {
    let mut stmt = conn.prepare("SELECT parent_key, cuts FROM splits WHERE position_idx=?1")?;
    let rows = stmt.query_map(params![idx as i64], |r| {
        let cuts_json: String = r.get(1)?;
        let cuts: Vec<usize> = serde_json::from_str(&cuts_json).unwrap_or_default();
        Ok(Split {
            parent_key: r.get(0)?,
            cuts,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn attach_decisions(
    candidates: &mut Vec<Candidate>,
    conn: &Connection,
    idx: usize,
) -> Result<(), AppError> {
    let live: std::collections::HashSet<String> =
        candidates.iter().map(|c| c.key.clone()).collect();
    let mut stmt = conn.prepare(
        "SELECT candidate_key, verdict, note, candidate_snapshot FROM decisions
         WHERE position_idx=?1",
    )?;
    let rows = stmt.query_map(params![idx as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (key, verdict, note, snapshot) = row?;
        let snap_val: Value = serde_json::from_str(&snapshot).unwrap_or(json!({}));
        if let Some(c) = candidates.iter_mut().find(|c| c.key == key) {
            c.serde_attach(verdict, note, None);
            c.snapshot_velocity_m_s = snap_val.get("velocity_m_s").and_then(|v| v.as_f64());
            c.snapshot_probe_delay_ns = snap_val.get("probe_delay_ns").and_then(|v| v.as_f64());
        } else if !live.contains(&key) {
            // Orphaned conclusion (threshold/gate changed): keep it with its snapshot.
            if let Ok(mut snap) = serde_json::from_str::<Value>(&snapshot) {
                snap["verdict"] = json!(verdict);
                snap["note"] = json!(note);
                snap["orphaned"] = json!(true);
                candidates.push(serde_json::from_value(snap)?);
            }
        }
    }
    Ok(())
}

fn load_tracks(conn: &Connection) -> Result<Vec<Value>, AppError> {
    let mut tracks = Vec::new();
    let mut tstmt = conn.prepare("SELECT id, name FROM tracks ORDER BY id")?;
    let ids: Vec<(i64, String)> = {
        let rows = tstmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        v
    };
    for (id, name) in ids {
        let mut mstmt = conn.prepare(
            "SELECT position_idx, candidate_key FROM track_members
             WHERE track_id=?1 ORDER BY ord",
        )?;
        let members: Vec<Value> = {
            let rows = mstmt.query_map(params![id], |r| {
                Ok(json!({"position_index": r.get::<_, i64>(0)?,
                          "candidate_key": r.get::<_, String>(1)?}))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        tracks.push(json!({"id": id, "name": name, "members": members}));
    }
    Ok(tracks)
}

fn log_event(conn: &Connection, etype: &str, payload: &Value) -> Result<i64, AppError> {
    conn.execute(
        "INSERT INTO events (ts, type, payload) VALUES (?1, ?2, ?3)",
        params![now_ts(), etype, payload.to_string()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn now_ts() -> String {
    // Seconds since epoch, UTC; deterministic format and no extra time crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn apply_fixture(conn: &Connection, fx_json: &str) -> Result<(), AppError> {
    let fx = Fixture::parse(fx_json).map_err(AppError)?;
    conn.execute_batch(
        "DELETE FROM track_members; DELETE FROM tracks; DELETE FROM splits;
         DELETE FROM decisions; DELETE FROM gates; DELETE FROM config;
         DELETE FROM positions;",
    )?;
    conn.execute("DELETE FROM kv WHERE k='fixture'", [])?;
    conn.execute(
        "INSERT INTO kv (k, v) VALUES ('fixture', ?1)",
        params![fx_json],
    )?;

    let cfg = AnalConfig {
        velocity_m_s: fx.velocity_m_s,
        probe_delay_ns: fx.probe_delay_ns,
        sample_interval_ns: fx.sample_interval_ns,
        min_resolvable_ns: fx.min_resolvable_ns,
        threshold_mode: ThresholdMode::Amplitude,
        amp_threshold: 0.15,
        snr_threshold: 4.0,
        noise_window: fx.noise_window,
    };
    conn.execute(
        "INSERT INTO config (id, v) VALUES (0, ?1)",
        params![serde_json::to_string(&cfg)?],
    )?;

    for (ord, g) in fx.gates.iter().enumerate() {
        conn.execute(
            "INSERT INTO gates (id,label,start_i,end_i,role,ord)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                g.id,
                g.label,
                g.start as i64,
                g.end as i64,
                g.role,
                ord as i64
            ],
        )?;
    }

    for (idx, p) in fx.positions.iter().enumerate() {
        let wave = crate::fixture::synthesize(&fx, idx);
        let col = (idx % fx.grid.cols) as f64 * fx.grid.dx_mm;
        let row = (idx / fx.grid.cols) as f64 * fx.grid.dy_mm;
        let surf = fx.surface_sample_for(p) as i64;
        conn.execute(
            "INSERT INTO positions (idx,id,name,x_mm,y_mm,wave,surface_sample)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                idx as i64,
                p.id,
                p.name,
                col,
                row,
                serde_json::to_string(&wave)?,
                surf
            ],
        )?;
    }
    Ok(())
}

fn read_config(conn: &Connection) -> Result<AnalConfig, AppError> {
    let v: String = conn.query_row("SELECT v FROM config WHERE id=0", [], |r| r.get(0))?;
    Ok(serde_json::from_str(&v)?)
}

fn read_fixture_meta(conn: &Connection) -> Result<(usize, f64), AppError> {
    let v: String = conn.query_row("SELECT v FROM kv WHERE k='fixture'", [], |r| r.get(0))?;
    let fx: Fixture = Fixture::parse(&v).map_err(AppError)?;
    Ok((fx.sample_count, fx.sample_interval_ns))
}

fn position_exists(conn: &Connection, idx: i64) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM positions WHERE idx=?1)",
        params![idx],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n != 0)
    .unwrap_or(false)
}

fn num_str(v: Option<&Value>, field: &str) -> Result<f64, AppError> {
    v.and_then(|x| x.as_f64())
        .ok_or_else(|| AppError(format!("missing numeric field `{field}`")))
}

fn apply_event(conn: &Connection, etype: &str, p: &Value) -> Result<(), AppError> {
    match etype {
        "fixture_imported" => {
            let fx_json = p
                .get("fixture")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError("fixture_imported requires `fixture` string".into()))?;
            apply_fixture(conn, fx_json)
        }
        "surface_corrected" => {
            let idx = num_str(p.get("position_index"), "position_index")? as i64;
            let sample = num_str(p.get("surface_sample"), "surface_sample")? as i64;
            if !position_exists(conn, idx) {
                return bad("unknown position_index");
            }
            let (n, _) = read_fixture_meta(conn)?;
            if sample < 0 || sample >= n as i64 {
                return bad("surface_sample out of range");
            }
            conn.execute(
                "UPDATE positions SET surface_sample=?1 WHERE idx=?2",
                params![sample, idx],
            )?;
            Ok(())
        }
        "config_updated" => {
            let mut cfg = read_config(conn)?;
            if let Some(v) = p.get("velocity_m_s").and_then(|x| x.as_f64()) {
                if v <= 0.0 {
                    return bad("velocity_m_s must be positive");
                }
                cfg.velocity_m_s = v;
            }
            if let Some(v) = p.get("probe_delay_ns").and_then(|x| x.as_f64()) {
                cfg.probe_delay_ns = v;
            }
            if let Some(v) = p.get("amp_threshold").and_then(|x| x.as_f64()) {
                if !(0.0..=1.0).contains(&v) {
                    return bad("amp_threshold must be within [0,1]");
                }
                cfg.amp_threshold = v;
            }
            if let Some(v) = p.get("snr_threshold").and_then(|x| x.as_f64()) {
                if v < 0.0 {
                    return bad("snr_threshold must be non-negative");
                }
                cfg.snr_threshold = v;
            }
            if let Some(v) = p.get("min_resolvable_ns").and_then(|x| x.as_f64()) {
                if v <= 0.0 {
                    return bad("min_resolvable_ns must be positive");
                }
                cfg.min_resolvable_ns = v;
            }
            if let Some(m) = p.get("threshold_mode").and_then(|x| x.as_str()) {
                cfg.threshold_mode = ThresholdMode::parse(m)
                    .ok_or_else(|| AppError("threshold_mode must be amplitude|snr".into()))?;
            }
            conn.execute(
                "UPDATE config SET v=?1 WHERE id=0",
                params![serde_json::to_string(&cfg)?],
            )?;
            Ok(())
        }
        "gate_updated" => {
            let id = p
                .get("gate_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError("gate_updated requires gate_id".into()))?;
            let start = num_str(p.get("start"), "start")? as i64;
            let end = num_str(p.get("end"), "end")? as i64;
            let (n, _) = read_fixture_meta(conn)?;
            if start < 0 || end <= start || end > n as i64 {
                return bad("gate must satisfy 0 <= start < end <= sample_count");
            }
            let affected = conn.execute(
                "UPDATE gates SET start_i=?1, end_i=?2 WHERE id=?3",
                params![start, end, id],
            )?;
            if affected == 0 {
                return bad("unknown gate_id");
            }
            Ok(())
        }
        _ => apply_event_decision(conn, etype, p),
    }
}

const VALID_VERDICTS: &[&str] = &[
    "retained",
    "true_scatterer",
    "saturation",
    "overlap",
    "rejected",
];

fn candidate_snapshot(conn: &Connection, idx: i64, key: &str) -> Result<Value, AppError> {
    let (wave_json, surf): (String, i64) = conn.query_row(
        "SELECT wave, surface_sample FROM positions WHERE idx=?1",
        params![idx],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let cfg = read_config(conn)?;
    let wave: Vec<f64> = serde_json::from_str(&wave_json)?;
    let mut gates: Vec<Gate> = {
        let mut stmt =
            conn.prepare("SELECT id,label,start_i,end_i,role FROM gates ORDER BY ord")?;
        let rows = stmt.query_map([], |r| {
            Ok(Gate {
                id: r.get(0)?,
                label: r.get(1)?,
                start: r.get::<_, i64>(2)? as usize,
                end: r.get::<_, i64>(3)? as usize,
                role: r.get(4)?,
            })
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        v
    };
    let splits = load_splits(conn, idx as usize)?;
    let analysis = analyze(&wave, &gates, &cfg, surf as usize, &splits);
    gates.clear();
    analysis
        .candidates
        .into_iter()
        .find(|c| c.key == key)
        .map(|c| serde_json::to_value(&c).unwrap_or(json!(null)))
        .ok_or_else(|| AppError("candidate not found for current run".to_string()))
}

fn apply_event_decision(conn: &Connection, etype: &str, p: &Value) -> Result<(), AppError> {
    match etype {
        "decision_made" => {
            let idx = num_str(p.get("position_index"), "position_index")? as i64;
            let key = p
                .get("candidate_key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError("decision requires candidate_key".into()))?;
            let verdict = p
                .get("verdict")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError("decision requires verdict".into()))?;
            if !VALID_VERDICTS.contains(&verdict) {
                return bad("invalid verdict");
            }
            if !position_exists(conn, idx) {
                return bad("unknown position_index");
            }
            let note = p.get("note").and_then(|v| v.as_str()).unwrap_or("");
            let snapshot = candidate_snapshot(conn, idx, key)?;
            conn.execute(
                "INSERT INTO decisions (position_idx,candidate_key,verdict,note,candidate_snapshot)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(position_idx,candidate_key)
                 DO UPDATE SET verdict=excluded.verdict, note=excluded.note,
                               candidate_snapshot=excluded.candidate_snapshot",
                params![idx, key, verdict, note, snapshot.to_string()],
            )?;
            Ok(())
        }
        "candidate_split" => {
            let idx = num_str(p.get("position_index"), "position_index")? as i64;
            let parent = p
                .get("parent_key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError("split requires parent_key".into()))?;
            let cuts: Vec<usize> = p
                .get("cuts")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as usize))
                        .collect()
                })
                .unwrap_or_default();
            if !position_exists(conn, idx) {
                return bad("unknown position_index");
            }
            conn.execute(
                "INSERT INTO splits (position_idx,parent_key,cuts) VALUES (?1,?2,?3)
                 ON CONFLICT(position_idx,parent_key) DO UPDATE SET cuts=excluded.cuts",
                params![idx, parent, serde_json::to_string(&cuts)?],
            )?;
            Ok(())
        }
        "track_put" => apply_track_put(conn, p),
        "track_deleted" => {
            let id = num_str(p.get("track_id"), "track_id")? as i64;
            conn.execute("DELETE FROM track_members WHERE track_id=?1", params![id])?;
            conn.execute("DELETE FROM tracks WHERE id=?1", params![id])?;
            Ok(())
        }
        other => bad(format!("unknown event type `{other}`")),
    }
}

fn member_pairs(p: &Value) -> Result<Vec<(i64, String)>, AppError> {
    let arr = p
        .get("members")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError("track requires a members array".into()))?;
    if arr.is_empty() {
        return bad("track must contain at least one member");
    }
    let mut pairs = Vec::new();
    for m in arr {
        let idx = num_str(m.get("position_index"), "position_index")? as i64;
        let key = m
            .get("candidate_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError("member requires candidate_key".into()))?
            .to_string();
        pairs.push((idx, key));
    }
    Ok(pairs)
}

fn apply_track_put(conn: &Connection, p: &Value) -> Result<(), AppError> {
    let pairs = member_pairs(p)?;
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("缺陷轨迹");
    let (_, fixture_json): (i64, String) =
        conn.query_row("SELECT 1, v FROM kv WHERE k='fixture'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let fx = Fixture::parse(&fixture_json).map_err(AppError)?;
    let cols = fx.grid.cols as i64;

    // Every referenced candidate must exist in the current analysis.
    for (idx, key) in &pairs {
        if !position_exists(conn, *idx) {
            return bad(format!("unknown position_index {idx}"));
        }
        candidate_snapshot(conn, *idx, key)?;
    }
    let existing_id = p.get("track_id").and_then(|v| v.as_i64());
    // Candidates must be unique across tracks (excluding the track being edited).
    for (idx, key) in &pairs {
        let mut q = conn.prepare(
            "SELECT track_id FROM track_members
             WHERE position_idx=?1 AND candidate_key=?2 AND track_id IS NOT ?3",
        )?;
        let other: Option<i64> = q
            .query_row(params![idx, key, existing_id.unwrap_or(-1)], |r| {
                r.get::<_, i64>(0)
            })
            .ok();
        if let Some(_tid) = other {
            return bad("candidate already belongs to a track");
        }
    }
    // Multi-member tracks must chain over 4-connected adjacent scan positions.
    if pairs.len() > 1 {
        for w in pairs.windows(2) {
            let (a, _) = w[0];
            let (b, _) = w[1];
            let ax = a % cols;
            let ay = a / cols;
            let bx = b % cols;
            let by = b / cols;
            let manhattan = (ax - bx).abs() + (ay - by).abs();
            if manhattan != 1 {
                return bad("track members must be adjacent scan positions (4-connected)");
            }
        }
    }

    let track_id = match existing_id {
        Some(id) => {
            let affected =
                conn.execute("DELETE FROM track_members WHERE track_id=?1", params![id])?;
            let _ = affected;
            conn.execute("UPDATE tracks SET name=?1 WHERE id=?2", params![name, id])?;
            id
        }
        None => {
            let seq = conn
                .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(0);
            conn.execute(
                "INSERT INTO tracks (name, created_seq) VALUES (?1, ?2)",
                params![name, seq],
            )?;
            conn.last_insert_rowid()
        }
    };
    for (ord, (idx, key)) in pairs.iter().enumerate() {
        conn.execute(
            "INSERT INTO track_members (track_id,position_idx,candidate_key,ord)
             VALUES (?1,?2,?3,?4)",
            params![track_id, idx, key, ord as i64],
        )?;
    }
    Ok(())
}
