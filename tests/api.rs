//! End-to-end HTTP tests against an in-memory database via the real router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use echo_bench::{app_state, db::Db, http::router};
use serde_json::{json, Value};
use tower::ServiceExt;

type App = axum::Router;

fn app() -> App {
    let db = Db::memory().expect("memory db");
    router(app_state(db))
}

async fn get(app: &App, uri: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 10_000_000)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

async fn call(app: &App, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 10_000_000)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let v = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, v)
}

fn state(v: &Value) -> &Value {
    v
}

fn candidates_at<'a>(s: &'a Value, idx: i64) -> &'a Value {
    &s["positions"][idx as usize]["analysis"]["candidates"]
}

fn find_cand<'a>(cands: &'a [Value], key: &str) -> Option<&'a Value> {
    cands.iter().find(|c| c["key"] == key)
}

#[tokio::test]
async fn root_page_has_title() {
    let res = app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 100_000)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("超声回波裁决台"));
}

#[tokio::test]
async fn fixed_fixture_boots_with_expected_features() {
    let (st, s) = get(&app(), "/api/state").await;
    assert_eq!(st, StatusCode::OK);
    let s = state(&s);
    assert_eq!(s["positions"].as_array().unwrap().len(), 15);
    // p2 (index 2): two unresolved near peaks -> one composite candidate.
    let p2 = candidates_at(s, 2).as_array().unwrap();
    let comp: Vec<_> = p2.iter().filter(|c| c["composite"] == true).collect();
    assert!(!comp.is_empty(), "p2 must retain a composite candidate");
    assert!(comp[0]["members"].as_array().unwrap().len() >= 2);
    // p8 (index 8): saturated plateau -> amplitude is a lower bound only.
    let p8 = candidates_at(s, 8).as_array().unwrap();
    let sat = p8
        .iter()
        .find(|c| c["saturated"] == true)
        .expect("saturated candidate");
    assert!(sat["amp"].as_f64().unwrap() >= 1.0 - 1e-6);
}

#[tokio::test]
async fn boundary_peak_belongs_to_one_gate_only() {
    let a = app();
    // Move the surface/inspection boundary to exactly the surface envelope peak.
    let (_, pre) = get(&a, "/api/state").await;
    let surf_peak = candidates_at(&pre, 0)
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["gate_id"] == json!("surface"))
        .unwrap()["peak_sample"]
        .as_u64()
        .unwrap() as i64;
    let (st, _) = call(
        &a,
        "PUT",
        "/api/gates/surface",
        json!({"start": 280, "end": surf_peak}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(
        &a,
        "PUT",
        "/api/gates/inspection",
        json!({"start": surf_peak, "end": 520}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s) = get(&a, "/api/state").await;
    let cands = candidates_at(&s, 0).as_array().unwrap();
    let on_boundary: Vec<_> = cands
        .iter()
        .filter(|c| c["peak_sample"] == json!(surf_peak))
        .collect();
    assert_eq!(on_boundary.len(), 1, "boundary peak appears once");
    // Left-closed/right-open: sample 300 belongs to inspection [300,520), not surface.
    assert_eq!(on_boundary[0]["gate_id"], json!("inspection"));
}

#[tokio::test]
async fn saturation_gives_lower_bound_but_not_false_split() {
    let (_, s) = get(&app(), "/api/state").await;
    let p8 = candidates_at(&s, 8).as_array().unwrap();
    let sat = p8.iter().find(|c| c["saturated"] == true).unwrap();
    // A single saturated platform must not be force-split into two points.
    assert!(!(sat["composite"] == true));
    assert!(sat["members"].as_array().map(|m| m.len()).unwrap_or(0) <= 1);
}

#[tokio::test]
async fn unresolved_peaks_remain_composite_and_can_be_split_manually() {
    let a = app();
    let (_, s) = get(&a, "/api/state").await;
    let p2 = candidates_at(&s, 2).as_array().unwrap();
    let comp = p2.iter().find(|c| c["composite"] == true).unwrap();
    let key = comp["key"].as_str().unwrap().to_string();
    let start = comp["start_sample"].as_u64().unwrap();
    let end = comp["end_sample"].as_u64().unwrap();
    let cut = ((start + end) / 2) as i64;
    let (st, _) = call(
        &a,
        "POST",
        &format!("/api/positions/2/candidates/{key}/split"),
        json!({"cuts": [cut]}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s2) = get(&a, "/api/state").await;
    let after = candidates_at(&s2, 2).as_array().unwrap();
    let kids: Vec<_> = after.iter().filter(|c| c["manual_split"] == true).collect();
    assert_eq!(kids.len(), 2, "manual split yields two children");
    assert!(kids.iter().all(|c| c["parent_key"] == json!(key)));
}

#[tokio::test]
async fn surface_later_than_bottom_blocks_depth() {
    let a = app();
    // Bottom candidate sits near sample 560; push surface arrival past it.
    let (st, body) = call(
        &a,
        "PUT",
        "/api/positions/0/surface",
        json!({"surface_sample": 580}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    let (_, s) = get(&a, "/api/state").await;
    let analysis = &s["positions"][0]["analysis"];
    assert_eq!(analysis["error"], json!("surface_after_bottom"));
    assert!(
        analysis["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["depth_mm"].is_null()),
        "no inverted depth results"
    );
    // Moving surface back before the bottom restores depth values.
    let (st, _) = call(
        &a,
        "PUT",
        "/api/positions/0/surface",
        json!({"surface_sample": 300}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s2) = get(&a, "/api/state").await;
    assert!(s2["positions"][0]["analysis"]["error"].is_null());
}

#[tokio::test]
async fn threshold_switch_changes_retained_candidates() {
    let a = app();
    // p7 carries a 0.10 sub-threshold scatterer at sample 470 under 0.15 amplitude.
    let (_, s) = get(&a, "/api/state").await;
    let before: Vec<f64> = candidates_at(&s, 7)
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["amp"].as_f64().unwrap())
        .collect();
    assert!(before.iter().all(|v| *v >= 0.15 - 1e-9));
    let (st, _) = call(
        &a,
        "PUT",
        "/api/config",
        json!({"threshold_mode": "amplitude", "amp_threshold": 0.05}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s2) = get(&a, "/api/state").await;
    let after_count = candidates_at(&s2, 7).as_array().unwrap().len();
    assert!(after_count >= before.len());
}

#[tokio::test]
async fn verdicts_keep_raw_window_velocity_and_delay() {
    let a = app();
    let (_, s) = get(&a, "/api/state").await;
    let cands = candidates_at(&s, 3).as_array().unwrap();
    let cand = cands
        .iter()
        .find(|c| c["gate_id"] == json!("inspection"))
        .expect("p3 inspection candidate");
    let key = cand["key"].as_str().unwrap();
    let (st, _) = call(
        &a,
        "PUT",
        &format!("/api/positions/3/candidates/{key}/decision"),
        json!({"verdict": "true_scatterer", "note": "确认缺陷"}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s2) = get(&a, "/api/state").await;
    let saved = find_cand(candidates_at(&s2, 3).as_array().unwrap(), key).unwrap();
    assert_eq!(saved["verdict"], json!("true_scatterer"));
    assert!(saved["start_sample"].is_number());
    assert!(saved["end_sample"].is_number());
    assert_eq!(saved["velocity_m_s"], json!(5900.0));
    assert_eq!(saved["probe_delay_ns"], json!(2000.0));
    // Change the velocity/depth-conversion version; conclusion remains recorded.
    let (st, _) = call(
        &a,
        "PUT",
        "/api/config",
        json!({"velocity_m_s": 6200.0, "probe_delay_ns": 2100.0}),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s3) = get(&a, "/api/state").await;
    let kept = find_cand(candidates_at(&s3, 3).as_array().unwrap(), key);
    assert!(
        kept.is_some(),
        "conclusion retained across velocity version"
    );
    assert_eq!(s3["config"]["velocity_m_s"], json!(6200.0));
    assert_eq!(s3["config"]["probe_delay_ns"], json!(2100.0));
    // The live candidate follows the new depth-conversion version for display...
    assert_eq!(kept.unwrap()["velocity_m_s"], json!(6200.0));
    // ...while the persisted conclusion snapshot retains the raw window and the
    // velocity/delay frozen at decision time for replay and audit.
    assert_eq!(kept.unwrap()["snapshot_velocity_m_s"], json!(5900.0));
    assert_eq!(kept.unwrap()["snapshot_probe_delay_ns"], json!(2000.0));
}

#[tokio::test]
async fn defect_track_requires_adjacent_positions() {
    let a = app();
    // p2 and p3 are horizontally adjacent and both carry inspection candidates.
    let (_, s) = get(&a, "/api/state").await;
    let k2 = candidates_at(&s, 2)
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["gate_id"] == json!("inspection"))
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let k3 = candidates_at(&s, 3)
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["gate_id"] == json!("inspection"))
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, body) = call(
        &a,
        "POST",
        "/api/tracks",
        json!({"name": "T", "members": [
            {"position_index": 2, "candidate_key": k2},
            {"position_index": 3, "candidate_key": k3}
        ]}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");

    // Non-adjacent members (p2 at (2,0) -> p8 at (3,1), distance 2) rejected.
    // Use a fresh app and read both keys from one state snapshot.
    let c = app();
    let (_, sc) = get(&c, "/api/state").await;
    let key_of = |idx: i64| {
        candidates_at(&sc, idx)
            .as_array()
            .unwrap()
            .iter()
            .find(|cc| cc["gate_id"] == json!("inspection"))
            .unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let kk2 = key_of(2);
    let kk8 = key_of(8);
    let (st, body) = call(
        &c,
        "POST",
        "/api/tracks",
        json!({"name": "bad", "members": [
            {"position_index": 2, "candidate_key": kk2},
            {"position_index": 8, "candidate_key": kk8}
        ]}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{}", body);
}

#[tokio::test]
async fn export_replay_reset_roundtrip_reproduces_state() {
    let a = app();
    // Produce some history.
    let (_, s) = get(&a, "/api/state").await;
    let key = candidates_at(&s, 3)
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["gate_id"] == json!("inspection"))
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &a,
        "PUT",
        &format!("/api/positions/3/candidates/{key}/decision"),
        json!({"verdict": "retained", "note": "roundtrip"}),
    )
    .await;
    call(&a, "PUT", "/api/config", json!({"amp_threshold": 0.2})).await;

    let (_, log) = get(&a, "/api/events").await;
    let events = log["events"].as_array().unwrap();
    assert!(events.len() >= 3);
    assert_eq!(events[0]["type"], json!("fixture_imported"));

    // Replay into a fresh database reproduces the same config and verdict.
    let b = app();
    let (st, r) = call(&b, "POST", "/api/replay", json!({"events": events})).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (_, sb) = get(&b, "/api/state").await;
    assert_eq!(sb["config"]["amp_threshold"], json!(0.2));
    let kept = find_cand(candidates_at(&sb, 3).as_array().unwrap(), &key).unwrap();
    assert_eq!(kept["verdict"], json!("retained"));
    assert_eq!(kept["note"], json!("roundtrip"));

    // Reset wipes back to the bundled fixture (one import event).
    let (st, _) = call(&a, "POST", "/api/reset", json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (_, log2) = get(&a, "/api/events").await;
    assert_eq!(log2["events"].as_array().unwrap().len(), 1);
    let (_, sa) = get(&a, "/api/state").await;
    assert_eq!(sa["config"]["amp_threshold"], json!(0.15));
}
