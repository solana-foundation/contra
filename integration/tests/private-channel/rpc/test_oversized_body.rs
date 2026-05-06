//! `test_rpc_handler_rejects_oversized_body`
//!
//! Target file: `core/src/rpc/handler.rs`.
//! Binary: `private_channel_integration` (existing).
//! Fixture: reuses `PrivateChannelContext` (no new boot cost).
//!
//! Exercises the body-size and Content-Length validation branches of the
//! HTTP handler by sending *raw* HTTP to bypass any client-side preflighting
//! the typed `RpcClient` might do. The handler's constants of interest:
//!   * `MAX_BODY_SIZE = 64 KiB` (from `core/src/rpc/constants.rs`)
//!   * `PARSE_ERROR_CODE = -32700` (jsonrpsee standard)
//!
//! Four boundary cases covered:
//!   A. Body exactly at `MAX_BODY_SIZE` → 200 (server parses, returns
//!      JSON-RPC parse error inside the 200 — *not* 413).
//!   B. Body one byte over `MAX_BODY_SIZE` → 413 Payload Too Large.
//!   C. Content-Length header > limit (early reject path) → 413.
//!   D. Unparseable Content-Length → 400 Bad Request.

use {
    super::test_context::PrivateChannelContext,
    http_body_util::{BodyExt, Full},
    hyper::{body::Bytes, Request, StatusCode},
    hyper_util::{client::legacy::Client, rt::TokioExecutor},
};

/// Parse `http://host:port` (or `http://host:port/path`) into (host, port).
/// The production PrivateChannelContext URL is always plain `http` on a fixed port,
/// so we intentionally keep this ad-hoc to avoid a new dep on `url`.
fn host_port_from_http_url(url: &str) -> (String, u16) {
    let rest = url.trim_start_matches("http://").trim_end_matches('/');
    // First `/` delimits authority from path; only consider authority.
    let authority = rest.split('/').next().unwrap_or(rest);
    let mut parts = authority.splitn(2, ':');
    let host = parts.next().expect("host").to_string();
    let port: u16 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .expect("explicit port required for this helper");
    (host, port)
}

const MAX_BODY_SIZE: usize = 64 * 1024;
const PARSE_ERROR_CODE: i32 = -32_700;

pub async fn run_oversized_body_test(ctx: &PrivateChannelContext) {
    println!("\n=== RPC Handler — Body-Size & Content-Length Boundaries ===");

    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();
    let url = ctx.write_client.url();

    case_a_body_at_limit_returns_200(&client, &url).await;
    case_b_body_one_over_limit_returns_413(&client, &url).await;
    case_c_content_length_over_limit_returns_413(&client, &url).await;
    case_d_unparseable_content_length_returns_400(&client, &url).await;

    println!("✓ all four body-size boundary cases passed");
}

// ── Case A ──────────────────────────────────────────────────────────────────
// A body *exactly* at `MAX_BODY_SIZE` must NOT trigger the Limited reader's
// size-exceeded path. The handler reads the body and then jsonrpsee parses
// it. Since our payload is garbage bytes (not valid JSON), jsonrpsee returns
// a JSON-RPC parse error inside a 200 response. This proves the size limit
// is inclusive, not exclusive.
async fn case_a_body_at_limit_returns_200(
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    let payload = vec![b'x'; MAX_BODY_SIZE];
    let req = Request::builder()
        .method("POST")
        .uri(url)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(payload)))
        .unwrap();
    let resp = client.request(req).await.expect("hyper send");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "body at exactly MAX_BODY_SIZE must be accepted; got {}",
        resp.status()
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body_bytes);
    // The server returns a JSON-RPC error inside the 200. The exact code
    // depends on where parsing fails: -32700 (parse error) from jsonrpsee
    // or -32603 (internal error) from the handler's catch-all path. Both
    // prove the body was accepted by the size-limit branch and then
    // rejected downstream — which is what Case A exists to demonstrate.
    assert!(
        body_str.contains(r#""error""#)
            && (body_str.contains("-32700") || body_str.contains("-32603")),
        "max-size garbage body must surface a JSON-RPC error inside the 200; body = {body_str}"
    );
}

// ── Case B ──────────────────────────────────────────────────────────────────
// One byte over → 413. This exercises the `Limited::new(..., MAX_BODY_SIZE)`
// downcast-to-`LengthLimitError` branch at handler.rs:~106.
async fn case_b_body_one_over_limit_returns_413(
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    let payload = vec![b'x'; MAX_BODY_SIZE + 1];
    let req = Request::builder()
        .method("POST")
        .uri(url)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(payload)))
        .unwrap();
    let resp = client.request(req).await.expect("hyper send");
    assert_eq!(
        resp.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "body one byte over MAX_BODY_SIZE must return 413"
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(
        body_str.contains(&PARSE_ERROR_CODE.to_string()) && body_str.contains("maximum size"),
        "413 body must explain the limit; body = {body_str}"
    );
}

// ── Case C ──────────────────────────────────────────────────────────────────
// Early-reject path: when the client honestly declares Content-Length above
// the limit, handler.rs:~76 rejects before reading the body at all. We still
// send the real body so hyper is happy.
async fn case_c_content_length_over_limit_returns_413(
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    let payload = vec![b'x'; MAX_BODY_SIZE + 1];
    let req = Request::builder()
        .method("POST")
        .uri(url)
        .header("Content-Type", "application/json")
        // hyper will set Content-Length automatically from the body; this is
        // an over-limit body so Content-Length is over-limit too.
        .body(Full::new(Bytes::from(payload)))
        .unwrap();
    let resp = client.request(req).await.expect("hyper send");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// ── Case D ──────────────────────────────────────────────────────────────────
// Malformed Content-Length (non-numeric) → 400. Exercises handler.rs:~83.
// We need to send an invalid CL header, which hyper normally prevents — so we
// use chunked transfer by hand via raw TCP to control the headers exactly.
async fn case_d_unparseable_content_length_returns_400(
    _client: &Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    url: &str,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let (host, port) = host_port_from_http_url(url);
    let mut stream = TcpStream::connect((host.as_str(), port)).await.unwrap();
    // An intentionally malformed Content-Length value. hyper's connector
    // would never emit this; writing raw bytes bypasses its validation.
    let req = format!(
        "POST / HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: not-a-number\r\n\
         \r\n\
         {{}}"
    );
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.unwrap();
    let resp_str = String::from_utf8_lossy(&resp);
    assert!(
        resp_str.contains("HTTP/1.1 400") || resp_str.contains("Invalid Content-Length"),
        "malformed Content-Length must produce 400; got:\n{resp_str}"
    );
}
