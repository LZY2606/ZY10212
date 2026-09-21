use std::net::SocketAddr;

use clap::Parser;
use echo_bench::{app_state, db::Db, http::router};

/// 超声回波裁决台 - local ultrasonic A-scan adjudication server.
#[derive(Parser, Debug)]
#[command(name = "echo-bench", version)]
struct Args {
    /// Listen address, e.g. 127.0.0.1:5552
    #[arg(long, default_value = "127.0.0.1:5552")]
    listen: String,
    /// SQLite database path (created and bootstrapped when empty).
    #[arg(long, default_value = "echo-bench.sqlite3")]
    db: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let db = Db::file(&args.db).expect("open database");
    let state = app_state(db);
    let app = router(state);

    let addr: SocketAddr = args
        .listen
        .parse()
        .unwrap_or_else(|_| panic!("invalid listen address {}", args.listen));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    println!(
        "超声回波裁决台 listening on http://{addr} (db: {})",
        args.db
    );
    axum::serve(listener, app).await.expect("server error");
}
