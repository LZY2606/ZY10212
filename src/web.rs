//! Axum router: JSON API plus the single-page adjudication console.

use crate::model::{apply, next_track_id, Event, World};
use crate::store::{now_rfc3339, Store};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
}

#[derive(Serialize)]
struct CandidateOut {
    #[serde(flatten)]
    c: crate::model::Candidate,
    live_depth_mm: Option<f64>,
    threshold_pass: bool,
}

#[derive(Serialize)]
struct GateOut {
    kind: String,
    lo: i64,
    hi: i64,
    lo_us: f64,
    hi_us: f64,
}

#[derive(Serialize)]
struct WorldLite {
    calibs: Vec<crate::fixture::Calib>,
    current_calib_id: i64,
    threshold_mode: String,
    threshold_amp: f64,
    threshold_snr: f64,
    inverted: bool,
    inverted_reason: Option<String>,
    fs_mhz: f64,
    n: usize,
    adc_max: i16,
    adc_scale: f64,
    gates: Vec<GateOut>,
    tracks: Vec<crate::model::Track>,
    next_track_id: i64,
    events: Vec<crate::model::EventRecord>,
    scans: Vec<ScanOut>,
}

#[derive(Serialize)]
struct ScanOut {
    id: i64,
    x_mm: f64,
    y_mm: f64,
    recorded_surface_sample: i64,
    surface_sample: i64,
    noise_floor: f64,
    /// Fixed display stride; candidates and fixture always carry full data.
    view_stride: usize,
    raw_view: Vec<i16>,
    envelope_view: Vec<f64>,
    candidates: Vec<CandidateOut>,
}

fn snapshot(store: &Store) -> Result<(World, Vec<crate::model::EventRecord>), String> {
    let log = store.load_log().map_err(|e| e.to_string())?;
    let mut log = log;
    let first = log
        .first()
        .ok_or_else(|| "missing bootstrap".to_string())?
        .1
        .clone();
    let mut world = match first {
        Event::Bootstrap {
            calib_id,
            mode,
            threshold_amp,
            threshold_snr,
        } => crate::model::boot(calib_id, &mode, threshold_amp, threshold_snr)?,
        _ => return Err("first event must be bootstrap".into()),
    };
    for (_seq, ev, _at, _note) in log.iter().skip(1) {
        apply(&mut world, ev)?;
    }
    let records = log
        .drain(..)
        .map(|(seq, event, at, note)| crate::model::EventRecord {
            seq,
            at_rfc3339: at,
            note,
            event,
        })
        .collect();
    Ok((world, records))
}

fn build_state(world: &World, events: Vec<crate::model::EventRecord>) -> WorldLite {
    let fs = crate::fixture::FS_MHZ;
    let thr_mode = world.threshold_mode.clone();
    let scans = world
        .scans
        .iter()
        .map(|sv| {
            let noise = sv.noise_floor;
            let candidates = world
                .candidates
                .iter()
                .filter(|c| c.scan_id == sv.id)
                .map(|c| {
                    let amp_norm = c.amplitude / crate::fixture::ADC_SCALE;
                    let threshold_pass = if thr_mode == "snr" {
                        amp_norm >= world.threshold_snr * noise
                    } else {
                        c.amplitude >= world.threshold_amp
                    };
                    CandidateOut {
                        c: c.clone(),
                        live_depth_mm: world.live_depth_mm(c.scan_id, c.peak),
                        threshold_pass,
                    }
                })
                .collect();
            const STRIDE: usize = 2;
            ScanOut {
                id: sv.id,
                x_mm: sv.x_mm,
                y_mm: sv.y_mm,
                recorded_surface_sample: sv.recorded_surface_sample,
                surface_sample: sv.surface_sample,
                noise_floor: sv.noise_floor,
                view_stride: STRIDE,
                raw_view: sv.raw.iter().step_by(STRIDE).copied().collect(),
                envelope_view: sv.envelope.iter().step_by(STRIDE).copied().collect(),
                candidates,
            }
        })
        .collect();
    WorldLite {
        calibs: world.calibs.clone(),
        current_calib_id: world.current_calib_id,
        threshold_mode: world.threshold_mode.clone(),
        threshold_amp: world.threshold_amp,
        threshold_snr: world.threshold_snr,
        inverted: world.inverted,
        inverted_reason: world.inverted_reason.clone(),
        fs_mhz: fs,
        n: crate::fixture::N,
        adc_max: crate::fixture::ADC_MAX,
        adc_scale: crate::fixture::ADC_SCALE,
        gates: world
            .gates
            .iter()
            .map(|g| GateOut {
                kind: g.kind.clone(),
                lo: g.lo,
                hi: g.hi,
                lo_us: g.lo as f64 / fs,
                hi_us: g.hi as f64 / fs,
            })
            .collect(),
        tracks: world.tracks.clone(),
        next_track_id: next_track_id(world),
        events,
        scans,
    }
}

async fn get_state(State(st): State<AppState>) -> Result<Json<WorldLite>, ApiError> {
    let guard = st.store.lock().unwrap();
    let (world, events) = snapshot(&guard).map_err(ApiError::bad)?;
    Ok(Json(build_state(&world, events)))
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn server(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct ActionBody {
    note: Option<String>,
    #[serde(flatten)]
    event: Event,
}

async fn post_event(
    State(st): State<AppState>,
    Json(body): Json<ActionBody>,
) -> Result<Json<WorldLite>, ApiError> {
    let mut store = st.store.lock().unwrap();
    let mut world = store.rehydrate().map_err(ApiError::bad)?;
    if let Event::Bootstrap { .. } = body.event {
        return Err(ApiError::bad("use POST /api/reset to re-bootstrap the log"));
    }
    apply(&mut world, &body.event).map_err(ApiError::bad)?;
    store
        .append(&body.event, &now_rfc3339(), body.note.as_deref())
        .map_err(|e| ApiError::server(e.to_string()))?;
    let (world, events) = snapshot(&store).map_err(ApiError::bad)?;
    Ok(Json(build_state(&world, events)))
}

#[derive(Deserialize)]
struct ResetBody {
    calib_id: Option<i64>,
    threshold_mode: Option<String>,
    threshold_amp: Option<f64>,
    threshold_snr: Option<f64>,
}

async fn reset(
    State(st): State<AppState>,
    body: Option<Json<ResetBody>>,
) -> Result<Json<WorldLite>, ApiError> {
    let b = body.map(|j| j.0);
    let ev = Event::Bootstrap {
        calib_id: b.as_ref().and_then(|x| x.calib_id).unwrap_or(1),
        mode: b
            .as_ref()
            .and_then(|x| x.threshold_mode.clone())
            .unwrap_or_else(|| "amp".into()),
        threshold_amp: b
            .as_ref()
            .and_then(|x| x.threshold_amp)
            .unwrap_or(crate::fixture::DEFAULT_THRESHOLD_AMP),
        threshold_snr: b
            .as_ref()
            .and_then(|x| x.threshold_snr)
            .unwrap_or(crate::fixture::DEFAULT_THRESHOLD_SNR),
    };
    let mut store = st.store.lock().unwrap();
    store.clear().map_err(|e| ApiError::server(e.to_string()))?;
    store
        .append(&ev, &now_rfc3339(), Some("reset"))
        .map_err(|e| ApiError::server(e.to_string()))?;
    let (world, events) = snapshot(&store).map_err(ApiError::bad)?;
    Ok(Json(build_state(&world, events)))
}

#[derive(Serialize)]
struct ExportDoc {
    format: String,
    version: u32,
    fixture: FixtureMeta,
    events: Vec<ExportEvent>,
}

#[derive(Serialize)]
struct FixtureMeta {
    samples: usize,
    fs_mhz: f64,
    f0_mhz: f64,
    adc_max: i16,
    calibs: Vec<crate::fixture::Calib>,
    sha256_of_waveforms: String,
}

#[derive(Serialize)]
struct ExportEvent {
    seq: i64,
    at_rfc3339: String,
    note: Option<String>,
    #[serde(flatten)]
    event: Event,
}

fn sha256_hex(data: &[u8]) -> String {
    // Small SHA-256 without an extra crate dependency.
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{:08x}", x)).collect()
}

async fn export(State(st): State<AppState>) -> Result<Response, ApiError> {
    let store = st.store.lock().unwrap();
    let log = store
        .load_log()
        .map_err(|e| ApiError::server(e.to_string()))?;
    let mut bytes: Vec<u8> = Vec::new();
    for s in crate::fixture::scans() {
        for v in &s.raw {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let doc = ExportDoc {
        format: "ultrasonic-adjudication-log".into(),
        version: 1,
        fixture: FixtureMeta {
            samples: crate::fixture::N,
            fs_mhz: crate::fixture::FS_MHZ,
            f0_mhz: crate::fixture::F0_MHZ,
            adc_max: crate::fixture::ADC_MAX,
            calibs: crate::fixture::CALIBS.to_vec(),
            sha256_of_waveforms: sha256_hex(&bytes),
        },
        events: log
            .into_iter()
            .map(|(seq, event, at, note)| ExportEvent {
                seq,
                at_rfc3339: at,
                note,
                event,
            })
            .collect(),
    };
    let body = serde_json::to_vec_pretty(&doc).map_err(|e| ApiError::server(e.to_string()))?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"adjudication-log.json\"",
            ),
        ],
        body,
    )
        .into_response())
}

#[derive(Deserialize)]
struct ImportDoc {
    events: Vec<ImportEvent>,
}

#[derive(Deserialize)]
struct ImportEvent {
    #[serde(flatten)]
    inner: serde_json::Value,
}

async fn import(
    State(st): State<AppState>,
    Json(doc): Json<ImportDoc>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let parsed: Vec<Event> = doc
        .events
        .iter()
        .enumerate()
        .map(|(i, ev)| {
            serde_json::from_value(ev.inner.clone())
                .map_err(|e| ApiError::bad(format!("event #{} is invalid: {}", i + 1, e)))
        })
        .collect::<Result<_, _>>()?;
    let first = parsed
        .first()
        .ok_or_else(|| ApiError::bad("import contains no events"))?;
    match first {
        Event::Bootstrap { .. } => {}
        _ => {
            return Err(ApiError::bad(
                "imported log must start with a bootstrap event",
            ))
        }
    }
    // Validate the whole stream before touching the database.
    let mut world = match first {
        Event::Bootstrap {
            calib_id,
            mode,
            threshold_amp,
            threshold_snr,
        } => crate::model::boot(*calib_id, mode, *threshold_amp, *threshold_snr)
            .map_err(ApiError::bad)?,
        _ => unreachable!(),
    };
    for ev in parsed.iter().skip(1) {
        apply(&mut world, ev).map_err(ApiError::bad)?;
    }
    let mut store = st.store.lock().unwrap();
    store.clear().map_err(|e| ApiError::server(e.to_string()))?;
    for ev in &parsed {
        store
            .append(ev, &now_rfc3339(), Some("imported"))
            .map_err(|e| ApiError::server(e.to_string()))?;
    }
    Ok(Json(
        serde_json::json!({ "imported": parsed.len(), "ok": true }),
    ))
}

async fn fixture_download() -> Response {
    let scans = crate::fixture::scans();
    let json = serde_json::json!({
        "samples": crate::fixture::N,
        "fs_mhz": crate::fixture::FS_MHZ,
        "f0_mhz": crate::fixture::F0_MHZ,
        "adc_max": crate::fixture::ADC_MAX,
        "adc_scale": crate::fixture::ADC_SCALE,
        "calibs": crate::fixture::CALIBS,
        "gates": crate::fixture::DEFAULT_GATES.iter().map(|(k,lo,hi)| serde_json::json!({"kind":k,"lo":lo,"hi":hi})).collect::<Vec<_>>(),
        "scans": scans,
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        serde_json::to_vec_pretty(&json).unwrap_or_default(),
    )
        .into_response()
}

async fn index() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("static/index.html"),
    )
        .into_response()
}

async fn app_js() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("static/app.js"),
    )
        .into_response()
}

pub fn router(mut store: Store) -> Router {
    store
        .rehydrate()
        .expect("seed and replay event log when building the router");
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
    };
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(get_state).post(post_event))
        .route("/api/reset", post(reset))
        .route("/api/export", get(export))
        .route("/api/import", post(import))
        .route("/api/fixture.json", get(fixture_download))
        .route("/app.js", get(app_js))
        .with_state(state)
}
