//! Standalone integration tests for the `/health` endpoint handled by
//! `handle_request` in `core/src/rpc/handler.rs`.
//!
//! Covers:
//!   * 200 happy path — never fires in this file (no RPC backend registered),
//!     so the real 200 coverage lives in the main `contra_integration`
//!     driver's `run_health_endpoint_test`.
//!   * 503 "degraded" — trivially induced by wiring an empty `RpcModule`:
//!     `getEpochSchedule` returns method-not-found, which the handler
//!     classifies as an unexpected-shape response and returns 503 degraded.
//!   * 404 fallthrough — unknown path & unsupported method both route to
//!     the catch-all arm.
//!
//! The "error" 503 branch (RPC itself throws) is skipped here: our in-process
//! RpcModule cannot be persuaded to `Err(_)` from inside `raw_json_request`
//! without a real backend dependency. The handler's own unit test
//! (`core/src/rpc/handler.rs::tests::health_returns_503_when_rpc_has_no_methods`)
//! already covers the shape-error case; this standalone file adds an
//! HTTP-level reproducer so coverage attributes correctly.

use {
    contra_core::rpc::handle_request,
    http_body_util::{BodyExt, Full},
    hyper::server::conn::http1,
    hyper::service::service_fn,
    hyper::{body::Bytes, Method, Request, StatusCode},
    hyper_util::{client::legacy::Client, rt::TokioExecutor, rt::TokioIo},
    jsonrpsee::server::RpcModule,
    std::sync::Arc,
    tokio::net::TcpListener,
};

async fn start_empty_server() -> std::net::SocketAddr {
    let rpc_module = Arc::new(RpcModule::new(()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            let rpc_module = Arc::clone(&rpc_module);
            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let rpc_module = Arc::clone(&rpc_module);
                    async move { handle_request(req, rpc_module).await }
                });
                let _ = http1::Builder::new().serve_connection(io, service).await;
            });
        }
    });

    addr
}

#[tokio::test]
async fn health_returns_503_when_backend_unavailable() {
    let addr = start_empty_server().await;
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    let url = format!("http://{addr}/health");
    let req = Request::builder()
        .method(Method::GET)
        .uri(&url)
        .body(Full::<Bytes>::new(Bytes::new()))
        .unwrap();
    let resp = client.request(req).await.expect("request must send");

    // Empty RpcModule → getEpochSchedule surfaces as an error or
    // method-not-found. Both take the handler into the 503 branch.
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "/health without a backend must report 503"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains(r#""status":"degraded""#) || body_str.contains(r#""status":"error""#),
        "503 body must carry a degraded/error status, got: {body_str}"
    );
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let addr = start_empty_server().await;
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    let url = format!("http://{addr}/does-not-exist");
    let req = Request::builder()
        .method(Method::GET)
        .uri(&url)
        .body(Full::<Bytes>::new(Bytes::new()))
        .unwrap();
    let resp = client.request(req).await.expect("request must send");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&*body, b"Not Found");
}

#[tokio::test]
async fn unsupported_method_on_root_returns_404() {
    let addr = start_empty_server().await;
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    let url = format!("http://{addr}/");
    let req = Request::builder()
        .method(Method::PUT)
        .uri(&url)
        .body(Full::<Bytes>::new(Bytes::from_static(b"{}")))
        .unwrap();
    let resp = client.request(req).await.expect("request must send");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
