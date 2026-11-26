//! Veridex server library — exposes the router and all API building blocks
//! so the binary and integration tests share the same wiring.

pub mod api;
pub mod authority;
pub mod error;
pub mod live;
pub mod pdf;
pub mod rank;
pub mod search;
pub mod state;
pub mod summarize;

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::path::PathBuf;
use tower_http::{
    cors::CorsLayer, services::ServeDir, trace::TraceLayer,
};

use state::AppState;

/// Builds the API-only router.
pub fn build_router(state: AppState) -> Router {
    build_router_with_static(state, None)
}

/// Builds the router, optionally serving the built frontend from `static_dir`.
///
/// The WebAssembly app uses hash routing, so plain static file serving needs
/// no SPA rewrite rules: `GET /` returns `index.html`, `/assets/*` the
/// bundles, and every API call stays under `/api`.
pub fn build_router_with_static(state: AppState, static_dir: Option<PathBuf>) -> Router {
    let router = Router::new()
        .route("/api", get(index))
        .route("/api/health", get(api::health))
        .route("/api/search", get(search::search))
        .route(
            "/api/summarize",
            post(summarize::summarize_post).get(summarize::summarize_get),
        )
        .route("/api/page/{id}", get(api::page))
        .route("/api/stats", get(api::stats))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    match static_dir {
        Some(dir) => router.fallback_service(ServeDir::new(dir)),
        None => router.route("/", get(index)),
    }
}

async fn index() -> Json<serde_json::Value> {
    Json(json!({
        "service": "veridex-server",
        "description": "Scholarly search & summarization API",
        "endpoints": {
            "GET  /api/health": "liveness probe",
            "GET  /api/search?q=<query>&page=<n>&limit=<n>&domain=<host>": "search",
            "POST /api/summarize": "JSON {query, max_pages, max_sentences, format: pdf|json, title}",
            "GET  /api/summarize?query=<q>": "summary as PDF download",
            "GET  /api/page/{id}?full=true": "page detail",
            "GET  /api/stats": "index statistics"
        }
    }))
}
