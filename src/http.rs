//! HTTP API and static Web UI.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use crate::err::AppError;
use crate::SharedDb;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({"error": self.0}))).into_response()
    }
}

pub fn router(state: SharedDb) -> Router {
    Router::new()
        .route("/api/state", get(get_state))
        .route("/api/config", put(put_config))
        .route("/api/gates/{gate_id}", put(put_gate))
        .route("/api/positions/{idx}/surface", put(put_surface))
        .route(
            "/api/positions/{idx}/candidates/{key}/decision",
            put(put_decision),
        )
        .route(
            "/api/positions/{idx}/candidates/{key}/split",
            post(post_split),
        )
        .route("/api/tracks", get(get_tracks).post(post_track))
        .route("/api/tracks/{id}", put(put_track).delete(delete_track))
        .route("/api/events", get(get_events))
        .route("/api/replay", post(post_replay))
        .route("/api/reset", post(post_reset))
        .route("/api/import-fixture", post(post_import_fixture))
        .fallback_service(ServeDir::new("static"))
        .with_state(state)
}

async fn get_state(State(db): State<SharedDb>) -> Result<Json<Value>, AppError> {
    Ok(Json(db.state()?))
}

async fn get_tracks(State(db): State<SharedDb>) -> Result<Json<Value>, AppError> {
    let s = db.state()?;
    Ok(Json(json!({"tracks": s["tracks"].clone()})))
}

async fn get_events(State(db): State<SharedDb>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({"events": db.export_events()?})))
}

#[derive(Deserialize, serde::Serialize)]
struct ConfigBody {
    velocity_m_s: Option<f64>,
    probe_delay_ns: Option<f64>,
    threshold_mode: Option<String>,
    amp_threshold: Option<f64>,
    snr_threshold: Option<f64>,
    min_resolvable_ns: Option<f64>,
}

async fn put_config(
    State(db): State<SharedDb>,
    Json(b): Json<ConfigBody>,
) -> Result<StatusCode, AppError> {
    db.cmd("config_updated", &serde_json::to_value(b).unwrap())?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct GateBody {
    start: usize,
    end: usize,
}

async fn put_gate(
    State(db): State<SharedDb>,
    axum::extract::Path(gate_id): axum::extract::Path<String>,
    Json(b): Json<GateBody>,
) -> Result<StatusCode, AppError> {
    db.cmd(
        "gate_updated",
        &json!({"gate_id": gate_id, "start": b.start, "end": b.end}),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SurfaceBody {
    surface_sample: i64,
}

async fn put_surface(
    State(db): State<SharedDb>,
    axum::extract::Path(idx): axum::extract::Path<i64>,
    Json(b): Json<SurfaceBody>,
) -> Result<StatusCode, AppError> {
    db.cmd(
        "surface_corrected",
        &json!({"position_index": idx, "surface_sample": b.surface_sample}),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DecisionBody {
    verdict: String,
    #[serde(default)]
    note: String,
}

async fn put_decision(
    State(db): State<SharedDb>,
    axum::extract::Path((idx, key)): axum::extract::Path<(i64, String)>,
    Json(b): Json<DecisionBody>,
) -> Result<StatusCode, AppError> {
    let allowed = [
        "retained",
        "true_scatterer",
        "saturation",
        "overlap",
        "rejected",
    ];
    if !allowed.contains(&b.verdict.as_str()) {
        return Err(AppError("invalid verdict".into()));
    }
    db.cmd(
        "decision_made",
        &json!({"position_index": idx, "candidate_key": key,
                "verdict": b.verdict, "note": b.note}),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SplitBody {
    cuts: Vec<i64>,
}

async fn post_split(
    State(db): State<SharedDb>,
    axum::extract::Path((idx, key)): axum::extract::Path<(i64, String)>,
    Json(b): Json<SplitBody>,
) -> Result<StatusCode, AppError> {
    let cuts: Vec<i64> = b
        .cuts
        .into_iter()
        .filter(|c| *c > 0)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    db.cmd(
        "candidate_split",
        &json!({"position_index": idx, "parent_key": key, "cuts": cuts}),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Clone)]
struct MemberBody {
    position_index: i64,
    candidate_key: String,
}

#[derive(Debug, Deserialize)]
struct TrackBody {
    name: Option<String>,
    members: Vec<MemberBody>,
}

fn track_payload(body: &TrackBody, track_id: Option<i64>) -> Value {
    let members: Vec<Value> = body
        .members
        .iter()
        .map(|m| json!({"position_index": m.position_index, "candidate_key": m.candidate_key}))
        .collect();
    let mut v = json!({"name": body.name.clone().unwrap_or_else(|| "缺陷轨迹".to_string()),
                       "members": members});
    if let Some(id) = track_id {
        v["track_id"] = json!(id);
    }
    v
}

async fn post_track(
    State(db): State<SharedDb>,
    Json(b): Json<TrackBody>,
) -> Result<Response, AppError> {
    let seq = db.cmd("track_put", &track_payload(&b, None))?;
    Ok((StatusCode::CREATED, Json(json!({"seq": seq}))).into_response())
}

async fn put_track(
    State(db): State<SharedDb>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(b): Json<TrackBody>,
) -> Result<StatusCode, AppError> {
    db.cmd("track_put", &track_payload(&b, Some(id)))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_track(
    State(db): State<SharedDb>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<StatusCode, AppError> {
    db.cmd("track_deleted", &json!({"track_id": id}))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ReplayBody {
    events: Vec<Value>,
}

async fn post_replay(
    State(db): State<SharedDb>,
    Json(b): Json<ReplayBody>,
) -> Result<Json<Value>, AppError> {
    let n = db.replay_export(&b.events)?;
    Ok(Json(json!({"replayed": n})))
}

async fn post_reset(State(db): State<SharedDb>) -> Result<Json<Value>, AppError> {
    let n = db.reset_to_bundled()?;
    Ok(Json(json!({"replayed": n})))
}

async fn post_import_fixture(
    State(db): State<SharedDb>,
    body: String,
) -> Result<Json<Value>, AppError> {
    // Accept either a raw fixture JSON string body, or {"fixture": "..."}.
    let fx_json = match serde_json::from_str::<Value>(&body) {
        Ok(Value::Object(map)) => match map.get("fixture") {
            Some(Value::String(s)) => s.clone(),
            _ => body.clone(),
        },
        _ => body.clone(),
    };
    // Validate before recording.
    crate::fixture::Fixture::parse(&fx_json).map_err(AppError)?;
    let n = db.replay_export(&[json!({"type": "fixture_imported",
                                     "payload": {"fixture": fx_json}})])?;
    Ok(Json(json!({"replayed": n})))
}
