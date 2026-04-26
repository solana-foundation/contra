//! Target file: `core/src/rpc/handler.rs` (health-check path + GET routing).
//! Binary: `contra_integration` (existing).
//! Fixture: reuses `ContraContext`.
//!
//! The handler's health check delegates to `getEpochSchedule` and returns:
//!   * `200 {"status":"ok"}`              when the RPC responds with a `result`.
//!   * `503 {"status":"degraded",...}`    when the RPC responds with something unexpected (shape parse failure).
//!   * `503 {"status":"error",...}`       when the RPC call itself errors.
//!
//! Exercises:
//!   A. GET /health returns 200 with `{"status":"ok"}` in the happy path.
//!   B. GET on an unknown path returns 404 Not Found (covers the `_ =>` arm).
//!   C. Unsupported method (e.g. PUT) on `/` returns 404.
//!
//! The "degraded" and "error" health branches rely on the underlying RPC
//! failing, which our in-process server cannot easily induce. Those branches
//! are covered by unit tests in `core/src/rpc/handler.rs::tests`, so this
//! integration test focuses on the three HTTP-routing branches that require
//! a real HTTP stack.

use {
    super::test_context::ContraContext,
    http_body_util::{BodyExt, Full},
    hyper::{body::Bytes, Method, Request, StatusCode},
    hyper_util::{client::legacy::Client, rt::TokioExecutor},
};

pub async fn run_health_endpoint_test(ctx: &ContraContext) {
    println!("\n=== RPC Handler — Health & Routing ===");

    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();
    let url = ctx.read_client.url();

    case_a_health_ok(&client, &url).await;
    case_b_unknown_path_404(&client, &url).await;
    case_c_unsupported_method_404(&client, &url).await;

    println!("✓ health endpoint + routing branches passed");
}

// ── Case A ──────────────────────────────────────────────────────────────────
async fn case_a_health_ok(
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    let health_url = format!("{}/health", url.trim_end_matches('/'));
    let req = Request::builder()
        .method(Method::GET)
        .uri(&health_url)
        .body(Full::<Bytes>::new(Bytes::new()))
        .unwrap();
    let resp = client.request(req).await.expect("hyper send");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "GET /health must be 200 when the node is healthy"
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(
        body_str.contains(r#""status":"ok""#),
        "healthy body must contain status:ok, got: {body_str}"
    );
}

// ── Case B ──────────────────────────────────────────────────────────────────
async fn case_b_unknown_path_404(
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    let unknown_url = format!("{}/does-not-exist", url.trim_end_matches('/'));
    let req = Request::builder()
        .method(Method::GET)
        .uri(&unknown_url)
        .body(Full::<Bytes>::new(Bytes::new()))
        .unwrap();
    let resp = client.request(req).await.expect("hyper send");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown path must be 404"
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&*body_bytes, b"Not Found");
}

// ── Case C ──────────────────────────────────────────────────────────────────
async fn case_c_unsupported_method_404(
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    let req = Request::builder()
        .method(Method::PUT)
        .uri(url)
        .body(Full::<Bytes>::new(Bytes::from_static(b"{}")))
        .unwrap();
    let resp = client.request(req).await.expect("hyper send");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unsupported method on `/` must be 404"
    );
}
