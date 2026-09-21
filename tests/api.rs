use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use ultrasonic_adjudication::router;
use ultrasonic_adjudication::store::Store;

fn app() -> axum::Router {
    router(Store::memory().unwrap())
}

async fn send(
    router: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(path);
    let req = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

async fn fresh() -> (axum::Router, Value) {
    let r = app();
    let (st, v) = send(&r, "GET", "/api/state", None).await;
    assert_eq!(st, StatusCode::OK, "{:?}", v);
    (r, v)
}

fn scan_candidates<'a>(state: &'a Value, scan_id: i64) -> Vec<&'a Value> {
    state["scans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == scan_id)
        .unwrap()["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .collect()
}

fn find_candidate<'a>(state: &'a Value, id: &str) -> &'a Value {
    state["scans"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["candidates"].as_array().unwrap().iter())
        .find(|c| c["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn page_title_and_fixture() {
    let r = app();
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = r.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&bytes);
    assert!(html.contains("超声回波裁决台"));
}

#[tokio::test]
async fn saturated_is_lower_bound_and_composite_kept() {
    let (_r, state) = fresh().await;
    // scan 5: saturated platform
    let sat = scan_candidates(&state, 5)
        .into_iter()
        .find(|c| c["kind"] == "saturated")
        .expect("saturated candidate");
    assert_eq!(sat["lower_bound"], json!(true));
    assert!(sat["amplitude"].as_f64().unwrap() >= 32_000.0);
    assert!(format!("{:?}", sat).contains("lower_bound"));

    // scan 3: two unresolved near peaks kept as one composite candidate
    let comps: Vec<_> = scan_candidates(&state, 3)
        .into_iter()
        .filter(|c| c["kind"] == "composite")
        .collect();
    assert_eq!(
        comps.len(),
        1,
        "unresolved near peaks must stay composite: {:?}",
        comps
    );
    let comp = comps[0];
    assert!(comp["sub_peaks"].as_array().unwrap().len() >= 2);
    assert_eq!(comp["gate_kind"], json!("inspection"));
    let depth = comp["live_depth_mm"].as_f64().unwrap();
    assert!((depth - 12.0).abs() < 0.5, "depth {}", depth);
}

#[tokio::test]
async fn half_open_gate_boundary_peak_belongs_to_one_gate() {
    let (r, state) = fresh().await;
    // Move the inspection/bottom boundary exactly onto a peak. The bottom
    // echo peaks at sample 879; set inspection hi = 879 so the peak belongs
    // ONLY to the bottom gate ([879,960)) and never to inspection.
    let bottom_peak = scan_candidates(&state, 1)
        .into_iter()
        .find(|c| c["gate_kind"] == "bottom")
        .unwrap()["peak"]
        .as_i64()
        .unwrap();
    assert_eq!(bottom_peak, 879);

    let (st, state) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"gate_moved","kind":"inspection","lo":350,"hi":bottom_peak})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{:?}", state);
    let owners: Vec<String> = scan_candidates(&state, 1)
        .into_iter()
        .filter(|c| c["peak"] == json!(bottom_peak))
        .map(|c| c["gate_kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        owners,
        vec!["bottom".to_string()],
        "boundary peak appears twice or in wrong gate"
    );

    // Move the boundary one sample to the right: peak 879 is now inside
    // [350,880) and therefore belongs ONLY to the inspection gate.
    let (_st2, state2) = send(
        &app(),
        "POST",
        "/api/state",
        Some(json!({"type":"gate_moved","kind":"inspection","lo":350,"hi":bottom_peak + 1})),
    )
    .await;
    let owners2: Vec<String> = scan_candidates(&state2, 1)
        .into_iter()
        .filter(|c| c["peak"] == json!(bottom_peak))
        .map(|c| c["gate_kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(owners2, vec!["inspection".to_string()]);
}

#[tokio::test]
async fn splitting_saturated_is_rejected() {
    let (r, state) = fresh().await;
    let sat_id = scan_candidates(&state, 5)
        .into_iter()
        .find(|c| c["kind"] == "saturated")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, v) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"candidate_split","parent_id":sat_id,"cut":608})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("saturated"));
}

#[tokio::test]
async fn wide_echo_splits_into_two_and_inversion_blocks() {
    let (r, state) = fresh().await;
    // scan 4 holds one wide single echo that is splittable.
    let wide = scan_candidates(&state, 4)
        .into_iter()
        .find(|c| c["gate_kind"] == "inspection" && c["kind"] == "single")
        .expect("wide echo");
    let parent = wide["id"].as_str().unwrap().to_string();
    let cut = (wide["start"].as_i64().unwrap() + wide["end"].as_i64().unwrap()) / 2;

    let (st, state2) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"candidate_split","parent_id":parent,"cut":cut})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{:?}", state2);
    let kids: Vec<_> = scan_candidates(&state2, 4)
        .into_iter()
        .filter(|c| c["origin"] == "split")
        .collect();
    assert_eq!(kids.len(), 2, "split must produce exactly two candidates");
    assert_eq!(
        kids[0]["end"], kids[1]["start"],
        "kids share the cut with half-open intervals"
    );
    for k in &kids {
        assert_eq!(k["calib_id_snapshot"], json!(1));
        assert!(k["depth_snapshot_mm"].is_number());
    }

    // Invert depth: push the corrected surface later than the bottom candidate,
    // continuing on the same router/log that already holds the split.
    let bottom_peak = scan_candidates(&state2, 4)
        .into_iter()
        .find(|c| c["gate_kind"] == "bottom")
        .unwrap()["peak"]
        .as_i64()
        .unwrap();
    let (st3, stt3) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"surface_corrected","scan_id":4,"sample":bottom_peak + 10})),
    )
    .await;
    assert_eq!(st3, StatusCode::OK);
    assert_eq!(stt3["inverted"], json!(true));
    let (st4, errv) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"candidate_verdict","candidate_id":"s4-c1","verdict":"scatter"})),
    )
    .await;
    assert_eq!(st4, StatusCode::BAD_REQUEST, "{:?}", errv);
    assert!(errv["error"].as_str().unwrap().contains("inverted"));
}

#[tokio::test]
async fn defect_track_requires_adjacent_positions() {
    let (r, state) = fresh().await;
    // defect candidates at scans 1 and 2 are adjacent.
    let id1 = find_candidate(&state, "s1-c2")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let id2 = find_candidate(&state, "s2-c2")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, v) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"track_grouped","track_id":1,"candidate_ids":[id1,id2]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{:?}", v);
    assert_eq!(v["tracks"][0]["candidate_ids"][0], json!("s1-c2"));

    // Same scan twice is rejected.
    let (st2, _v2) = send(
        &app(),
        "POST",
        "/api/state",
        Some(json!({"type":"track_grouped","track_id":2,"candidate_ids":["s1-c1","s1-c2"]})),
    )
    .await;
    assert_eq!(st2, StatusCode::BAD_REQUEST);
    assert!(_v2["error"].as_str().unwrap().contains("position"));
}

#[tokio::test]
async fn threshold_switch_changes_candidates() {
    let (r, state) = fresh().await;
    let before = state["scans"][0]["candidates"].as_array().unwrap().len();
    let (st, v) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"threshold_changed","mode":"snr","value":60.0})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{:?}", v);
    let after = v["scans"][0]["candidates"].as_array().unwrap().len();
    assert!(
        after < before,
        "a high SNR threshold must suppress candidates"
    );
}

#[tokio::test]
async fn export_reset_import_roundtrip() {
    let r = app();
    // make a few operations
    let (s1, _) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"surface_corrected","scan_id":2,"sample":205})),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    let (s2, v2) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"candidate_verdict","candidate_id":"s1-c2","verdict":"scatter"})),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);

    let req = Request::builder()
        .uri("/api/export")
        .body(Body::empty())
        .unwrap();
    let resp = r.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc["format"], json!("ultrasonic-adjudication-log"));
    assert_eq!(doc["events"].as_array().unwrap().len(), 3);
    assert!(doc["fixture"]["sha256_of_waveforms"].is_string());

    // Clear then import on a fresh database-backed store.
    let r2 = app();
    let (sr, sv) = send(&r2, "POST", "/api/reset", Some(json!({}))).await;
    assert_eq!(sr, StatusCode::OK);
    assert_eq!(sv["events"].as_array().unwrap().len(), 1);

    let body = json!({"events": doc["events"]});
    let (ir, iv) = send(&r2, "POST", "/api/import", Some(body)).await;
    assert_eq!(ir, StatusCode::OK, "{:?}", iv);
    assert_eq!(iv["imported"], json!(3));

    let (_gr, replay) = send(&r2, "GET", "/api/state", None).await;
    assert_eq!(replay["events"].as_array().unwrap().len(), 3);
    let c = find_candidate(&replay, "s1-c2");
    assert_eq!(c["verdict"], json!("scatter"));
    assert_eq!(replay["scans"][1]["surface_sample"], json!(205));
}

#[tokio::test]
async fn each_candidate_keeps_sample_interval_and_calibration() {
    let (_r, state) = fresh().await;
    for s in state["scans"].as_array().unwrap() {
        for c in s["candidates"].as_array().unwrap() {
            assert!(c["start"].is_number() && c["end"].is_number());
            assert!(c["end"].as_i64().unwrap() > c["start"].as_i64().unwrap());
            assert!(c["calib_id_snapshot"].is_number());
        }
    }
}

#[tokio::test]
async fn calibration_switch_updates_live_depth_but_keeps_snapshot() {
    let (r, state) = fresh().await;
    let before = find_candidate(&state, "s1-c2");
    let snap_before = before["depth_snapshot_mm"].as_f64().unwrap();
    assert_eq!(before["calib_id_snapshot"], json!(1));

    // Verdict locks the candidate and records its calibration snapshot.
    let (st, v) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"candidate_verdict","candidate_id":"s1-c2","verdict":"scatter"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{:?}", v);

    // Switching to the second velocity/delay version changes live depth.
    let (st2, v2) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"calibration_switched","calib_id":2})),
    )
    .await;
    assert_eq!(st2, StatusCode::OK, "{:?}", v2);
    assert_eq!(v2["current_calib_id"], json!(2));
    let after = find_candidate(&v2, "s1-c2");
    // Snapshot is immutable; live depth uses the new velocity.
    assert_eq!(after["depth_snapshot_mm"], json!(snap_before));
    assert_eq!(after["calib_id_snapshot"], json!(1));
    let live2 = after["live_depth_mm"].as_f64().unwrap();
    let expected2 = 0.5_f64
        * 5920.0
        * (after["peak"].as_i64().unwrap() - v2["scans"][0]["surface_sample"].as_i64().unwrap())
            as f64
        / (100.0 * 1.0e6)
        * 1000.0;
    assert!(
        (live2 - expected2).abs() < 1e-6,
        "{} vs {}",
        live2,
        expected2
    );
    // Unknown calibration rejected.
    let (st3, v3) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"calibration_switched","calib_id":99})),
    )
    .await;
    assert_eq!(st3, StatusCode::BAD_REQUEST);
    assert!(v3["error"].as_str().unwrap().contains("calibration"));
}

#[tokio::test]
async fn invalid_events_are_rejected_before_persisting() {
    let (r, _) = fresh().await;
    // Out-of-range surface sample.
    let (st, v) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"surface_corrected","scan_id":1,"sample":99999})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // Reversed gate interval.
    let (st2, v2) = send(
        &r,
        "POST",
        "/api/state",
        Some(json!({"type":"gate_moved","kind":"surface","lo":400,"hi":300})),
    )
    .await;
    assert_eq!(st2, StatusCode::BAD_REQUEST);
    assert!(v2["error"].as_str().unwrap().contains("gate"));
    // Event log still only contains bootstrap.
    let (_g, state) = send(&r, "GET", "/api/state", None).await;
    assert_eq!(state["events"].as_array().unwrap().len(), 1);
}
