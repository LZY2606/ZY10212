use clap::Parser;
use std::net::SocketAddr;
use ultrasonic_adjudication::router as build_router;
use ultrasonic_adjudication::store::Store;

#[derive(Parser, Debug)]
#[command(name = "ultrasonic-adjudication", about = "超声回波裁决台")]
struct Args {
    /// Address to listen on, e.g. 127.0.0.1:5552
    #[arg(long, default_value = "127.0.0.1:5552")]
    listen: String,

    /// SQLite database path. Use :memory: for an ephemeral database.
    #[arg(long, default_value = "adjudication.db")]
    db: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let addr: SocketAddr = args
        .listen
        .parse()
        .unwrap_or_else(|e| panic!("invalid --listen {}: {}", args.listen, e));

    let store = if args.db == ":memory:" {
        Store::memory().expect("open in-memory sqlite")
    } else {
        Store::open(&args.db).unwrap_or_else(|e| panic!("open sqlite {}: {}", args.db, e))
    };
    {
        let mut seeded = store;
        seeded.rehydrate().expect("seed/ replay event log");
        let app = build_router(seeded);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|e| panic!("bind {}: {}", addr, e));
        println!(
            "超声回波裁决台 listening on http://{} (ctrl-c to stop)",
            addr
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .expect("server error");
    }
}
