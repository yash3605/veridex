//! HTTP integration tests: the full axum router against an in-memory index.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use veridex_core::db;
use veridex_server::{build_router, state::AppState};

async fn fixture_app() -> axum::Router {
    let pool = db::connect(":memory:").await.expect("in-memory db");
    db::init_schema(&pool).await.expect("schema");

    db::insert_page(
        &pool,
        "https://plato.stanford.edu/entries/photosynthesis/",
        Some("plato.stanford.edu"),
        Some("Photosynthesis (Stanford Encyclopedia of Philosophy)".into()),
        Some("A scholarly overview of photosynthesis research.".into()),
        Some("photosynthesis, biology".into()),
        Some(
            "Photosynthesis converts light energy into chemical energy. \
             The light-dependent reactions occur in the thylakoid membrane. \
             Chlorophyll absorbs photons near 430 and 662 nanometres. \
             The Calvin cycle fixes carbon dioxide into glyceraldehyde 3-phosphate. \
             Researchers measured a quantum yield of 0.125 under controlled conditions. \
             Isotope tracing experiments confirm the temporal ordering of metabolites. \
             Global crop yields depend on the efficiency of these reactions worldwide. \
             Computational models estimate theoretical maximum efficiency near eleven percent. \
             Field measurements consistently report six to eight percent for cereal crops. \
             Temperature stress deactivates rubisco activase during heavy summer heat. \
             Recent advances in cryo electron microscopy reveal conformational states. \
             These insights explain how excitation energy transfers with minimal loss. \
             The full pathway was elucidated through decades of biochemical work. \
             Water photolysis supplies electrons that replenish photosystem two centres."
                .into(),
        ),
        "<html></html>".into(),
    )
    .await
    .unwrap();

    db::insert_page(
        &pool,
        "https://wikihow.com/to-do-photosynthesis",
        Some("wikihow.com"),
        Some("How to Do Photosynthesis".into()),
        None,
        None,
        Some(
            "Photosynthesis is when plants eat sunlight maybe. \
             Anyway here are ten amazing photosynthesis facts that will blow your mind. \
             Number seven is absolutely wild and totally scientific for sure."
                .into(),
        ),
        "<html></html>".into(),
    )
    .await
    .unwrap();

    db::insert_page(
        &pool,
        "https://arxiv.org/abs/2401.00001",
        Some("arxiv.org"),
        Some("Attention mechanisms for photosynthesis modelling".into()),
        None,
        None,
        Some(
            "We present a transformer model that predicts photosynthesis rates from \
             leaf spectroscopy data. Our architecture achieves state of the art accuracy \
             on three public benchmarks and generalises across climates well. \
             Experiments show the attention mechanism focuses on absorption bands. \
             Ablation studies confirm each component contributes measurably overall. \
             Code and datasets are released publicly for the research community to use."
                .into(),
        ),
        "<html></html>".into(),
    )
    .await
    .unwrap();

    build_router(AppState { pool })
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn health_endpoint() {
    let app = fixture_app().await;
    let (status, body) = get_json(&app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn search_returns_ranked_results_with_transparency() {
    let app = fixture_app().await;
    let (status, body) = get_json(&app, "/api/search?q=photosynthesis").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 3, "all three fixture pages match: {body}");

    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);

    // The .edu-adjacent scholarly source must rank first despite arxiv
    // having a matching title — authority dominates ties.
    assert!(
        results[0]["url"]
            .as_str()
            .unwrap()
            .contains("stanford.edu"),
        "first result: {body}"
    );
    assert_eq!(results[0]["grade"], "A");

    // Every result exposes score components for UI transparency.
    for r in results {
        assert!(r["score"].as_f64().unwrap() > 0.0);
        assert!(r["relevance"].as_f64().unwrap() >= 0.0);
        assert!(r["authority"].as_f64().unwrap() > 0.0);
        assert!(r["grade"].is_string());
        assert!(r["signals"].is_array());
        assert!(!r["snippet"].as_str().unwrap().is_empty());
    }

    // The content farm must be graded down.
    let farm = results
        .iter()
        .find(|r| r["url"].as_str().unwrap().contains("wikihow"))
        .expect("wikihow result present");
    assert_eq!(farm["grade"], "D");
}

#[tokio::test]
async fn search_site_filter_restricts_to_host() {
    let app = fixture_app().await;
    let (status, body) =
        get_json(&app, "/api/search?q=photosynthesis&domain=arxiv.org").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert!(body["results"][0]["url"]
        .as_str()
        .unwrap()
        .contains("arxiv.org"));

    // site: prefix in the query itself works too.
    let (_, body) = get_json(&app, "/api/search?q=photosynthesis+site:wikihow.com").await;
    assert_eq!(body["total"], 1);
    assert!(body["results"][0]["url"]
        .as_str()
        .unwrap()
        .contains("wikihow"));
}

#[tokio::test]
async fn search_handles_junk_queries() {
    let app = fixture_app().await;
    let (status, body) = get_json(&app, "/api/search?q=%21%21%21").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);

    let (status, body) = get_json(&app, "/api/search?q=zzznotindexedzzz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn page_detail_and_404() {
    let app = fixture_app().await;
    let (status, body) = get_json(&app, "/api/search?q=photosynthesis").await;
    assert_eq!(status, StatusCode::OK);
    let id = body["results"][0]["id"].as_i64().unwrap();

    let (status, page) = get_json(&app, &format!("/api/page/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["grade"], "A");
    assert!(page["content"].as_str().unwrap().contains("Photosynthesis"));
    assert_eq!(page["truncated"], false);

    let (status, _) = get_json(&app, "/api/page/999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_endpoint_reports_index() {
    let app = fixture_app().await;
    let (status, body) = get_json(&app, "/api/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pages"], 3);
    let hosts = body["top_hosts"].as_array().unwrap();
    assert!(hosts.iter().any(|h| h["host"] == "arxiv.org"));
}

async fn post_json(app: &axum::Router, uri: &str, payload: Value) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn summarize_returns_json() {
    let app = fixture_app().await;
    let (status, bytes) = post_json(
        &app,
        "/api/summarize",
        json!({"query": "photosynthesis", "format": "json"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(!doc["findings"].as_array().unwrap().is_empty());
    assert!(!doc["sources"].as_array().unwrap().is_empty());
    assert_eq!(doc["sources"][0]["grade"], "A");
    assert_eq!(doc["sources"][0]["url"], "https://plato.stanford.edu/entries/photosynthesis/");
}

#[tokio::test]
async fn summarize_returns_pdf_download() {
    let app = fixture_app().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/summarize")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"query": "photosynthesis", "format": "pdf"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(content_type, "application/pdf");
    let disposition = response
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.contains("veridex-summary-photosynthesis.pdf"));

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.starts_with(b"%PDF"));
}

#[tokio::test]
async fn summarize_unknown_query_is_404() {
    let app = fixture_app().await;
    let (status, bytes) = post_json(
        &app,
        "/api/summarize",
        json!({"query": "zzznotindexedzzz", "format": "json"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn summarize_get_returns_pdf() {
    let app = fixture_app().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/summarize?query=photosynthesis")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.starts_with(b"%PDF"));
}

// ---------------------------------------------------------------------------
// Live search: discovery + crawl-on-demand against local fixture servers
// (no external network — VERIDEX_DISCOVER_URL points at a mock endpoint).
// ---------------------------------------------------------------------------

const LIVE_TARGET_HTML: &str = r#"<!DOCTYPE html>
<html><head>
  <title>Quantum Coherence in Photosynthesis</title>
  <meta name="description" content="Quantum coherence in photosynthetic energy transfer.">
</head><body>
  <article>
    <h1>Quantum Coherence in Photosynthesis</h1>
    <p>Quantum coherence in photosynthetic energy transfer was observed at ambient
       temperature. The reaction centre exhibits long-lived electronic coherence.
       Exciton dynamics measured with two-dimensional spectroscopy support this.
       These quantum photosynthesis results reshape models of energy transport.</p>
  </article>
</body></html>"#;

#[tokio::test]
async fn live_search_fetches_stores_and_summarizes() {
    use axum::{Router, routing::get};

    // The "web": a target page plus an allow-all robots.txt.
    let target_app = Router::new()
        .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
        .route(
            "/entries/quantum-photosynthesis",
            get(|| async { axum::response::Html(LIVE_TARGET_HTML) }),
        );
    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind target");
    let target_addr = target_listener.local_addr().expect("target addr");
    tokio::spawn(async move {
        axum::serve(target_listener, target_app)
            .await
            .expect("target serve");
    });
    let target_url = format!("http://{target_addr}/entries/quantum-photosynthesis");

    // Discovery fixture: a DuckDuckGo-shaped results page pointing at it.
    let encoded: String = url::form_urlencoded::byte_serialize(target_url.as_bytes()).collect();
    let disc_html = format!(
        r#"<html><body>
         <a rel="nofollow" class="result__a"
            href="//duckduckgo.com/l/?uddg={encoded}&amp;rut=fixture">Result</a>
       </body></html>"#
    );
    let disc_app = Router::new().route(
        "/",
        post(move || {
            let html = disc_html.clone();
            async move { html }
        }),
    );
    let disc_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind discovery");
    let disc_addr = disc_listener.local_addr().expect("discovery addr");
    tokio::spawn(async move {
        axum::serve(disc_listener, disc_app)
            .await
            .expect("discovery serve");
    });

    // SAFETY: this is the only test that runs live search, hence the only
    // reader/writer of VERIDEX_DISCOVER_URL; no other test touches it.
    unsafe {
        std::env::set_var(
            "VERIDEX_DISCOVER_URL",
            format!("http://{disc_addr}/"),
        );
    }

    let app = fixture_app().await;
    let (status, before) = get_json(&app, "/api/stats").await;
    assert_eq!(status, StatusCode::OK);
    let pages_before = before["pages"].as_i64().expect("page count");

    // Without live, the page doesn't exist as far as search is concerned.
    let (status, local) = get_json(&app, "/api/search?q=quantum+photosynthesis").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(local["live"], false);
    assert_eq!(local["live_fetched"], 0);
    assert!(
        !local["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["url"] == target_url),
        "page must be absent before live search"
    );

    // Live: discover → fetch → store → rank with the hybrid formula.
    let (status, body) = get_json(&app, "/api/search?q=quantum+photosynthesis&live=true").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["live"], true);
    assert_eq!(
        body["live_fetched"], 1,
        "expected exactly one newly stored page: {body}"
    );
    let results = body["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|r| r["url"] == target_url),
        "live page missing from results: {body}"
    );
    assert!(results.iter().all(|r| r["grade"].is_string()));

    // Persisted: stats grew, and a repeat search must not refetch it.
    let (_, after) = get_json(&app, "/api/stats").await;
    assert_eq!(after["pages"].as_i64().unwrap(), pages_before + 1);
    let (_, again) = get_json(&app, "/api/search?q=quantum+photosynthesis&live=true").await;
    assert_eq!(
        again["live_fetched"], 0,
        "stored page must not be fetched twice: {again}"
    );

    // Research briefs can cite the live-fetched source.
    let (status, bytes) = post_json(
        &app,
        "/api/summarize",
        json!({"query": "quantum photosynthesis", "format": "json", "live": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: Value = serde_json::from_slice(&bytes).unwrap();
    let sources = summary["sources"].as_array().expect("sources array");
    assert!(
        sources.iter().any(|s| s["url"] == target_url),
        "brief must cite the live-fetched page: {summary}"
    );

    // SAFETY: paired with the set_var above; no other test reads this.
    unsafe { std::env::remove_var("VERIDEX_DISCOVER_URL") };
}
