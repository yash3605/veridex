//! Veridex server binary — parses CLI args, opens the shared database and
//! serves the API router.

use clap::Parser;
use veridex_server::{build_router_with_static, state::AppState};

#[derive(Parser, Debug)]
#[command(
    name = "veridex-server",
    about = "Backend API for the Veridex scholarly search engine"
)]
struct Args {
    /// Path to the SQLite database (shared with the crawler)
    #[arg(long, default_value = "crawled.db")]
    db: String,

    /// Bind address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Bind port
    #[arg(long, short, default_value_t = 8080)]
    port: u16,

    /// Optional directory with the built frontend (e.g. `frontend/dist`);
    /// when set, the app is served at `/` alongside the API.
    #[arg(long)]
    static_dir: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let pool = veridex_core::db::connect(&args.db).await?;
    veridex_core::db::init_schema(&pool).await?;
    let state = AppState { pool };

    let app = build_router_with_static(state, args.static_dir.clone());

    if let Some(dir) = args.static_dir {
        tracing::info!("serving frontend from {}", dir.display());
    }

    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("veridex-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
