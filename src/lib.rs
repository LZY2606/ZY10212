//! 超声回波裁决台 (Ultrasonic Echo Adjudication Workbench).

pub mod db;
pub mod err;
pub mod fixture;
pub mod http;
pub mod signal;

/// Fixed fixture bundled into the binary for offline deterministic replay.
pub const FIXTURE_JSON: &str = include_str!("../fixtures/echo_fixture.json");

use std::sync::Arc;

pub type SharedDb = Arc<db::Db>;

pub fn app_state(db: db::Db) -> SharedDb {
    Arc::new(db)
}
