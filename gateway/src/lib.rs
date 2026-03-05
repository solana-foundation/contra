pub mod metrics;

use clap::Parser;
use http_body_util::{BodyExt, Empty, Full, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use jsonrpsee::types::error::INVALID_REQUEST_CODE;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Maximum allowed request body size (64 KB).
const MAX_BODY_SIZE: usize = 64 * 1024;

const KNOWN_RPC_METHODS: &[&str] = &[
    "sendTransaction",
    "getAccountInfo",
    "getSlot",
    "getBlock",
    "getTransaction",
    "getRecentBlockhash",
    "getTokenAccountBalance",
    "getLatestBlockhash",
    "getSignatureStatuses",
    "getTransactionCount",
    "getFirstAvailableBlock",
    "getBlocks",
    "getEpochInfo",
    "getEpochSchedule",
    "getRecentPerformanceSamples",
    "getBlockTime",
    "getVoteAccounts",
    "getSupply",
    "getSlotLeaders",
    "isBlockhashValid",
    "simulateTransaction",
];

#[derive(Parser, Debug, Clone)]
#[command(name = "contra-gateway")]
#[command(about = "JSON RPC gateway that routes requests to write or read nodes")]
pub struct Args {
    /// Port to run the gateway on
    #[arg(short, long, env = "GATEWAY_PORT", default_value = "8898")]
    pub port: u16,

    /// Write node URL (for send_transaction requests)
    #[arg(short, long, env = "GATEWAY_WRITE_URL")]
    pub write_url: String,

    /// Read node URL (for all other requests)
    #[arg(short, long, env = "GATEWAY_READ_URL")]
    pub read_url: String,

    /// CORS Access-Control-Allow-Origin header value
    #[arg(long, default_value = "*", env = "GATEWAY_CORS_ALLOWED_ORIGIN")]
    pub cors_allowed_origin: String,
}

pub struct Gateway {
    write_url: String,
    read_url: String,
    cors_allowed_origin: String,
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
}

impl Gateway {
    pub fn new(write_url: String, read_url: String, cors_allowed_origin: String) -> Self {
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Self {
            write_url,
            read_url,
            cors_allowed_origin,
            client,
        }
    }

    fn record_metrics(
        error_type: Option<&str>,
        method: &str,
        target: &str,
        status: &str,
        elapsed: f64,
    ) {
        if let Some(et) = error_type {
            metrics::GATEWAY_ERRORS_TOTAL.with_label_values(&[et]).inc();
        }
        metrics::GATEWAY_REQUESTS_TOTAL
            .with_label_values(&[method, target, status])
            .inc();
        metrics::GATEWAY_REQUEST_DURATION
            .with_label_values(&[method, target])
            .observe(elapsed);
    }

    fn error_response(
        &self,
        status: StatusCode,
        body: Option<Bytes>,
    ) -> Response<http_body_util::combinators::UnsyncBoxBody<Bytes, hyper::Error>> {
        let mut builder = Response::builder().status(status).header(
            "Access-Control-Allow-Origin",
            self.cors_allowed_origin.as_str(),
        );
        match body {
            Some(bytes) => {
                builder = builder.header("Content-Type", "application/json");
                builder
                    .body(
                        Full::new(bytes)
                            .map_err(|never| match never {})
                            .boxed_unsync(),
                    )
                    .unwrap()
            }
            None => builder
                .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                .unwrap(),
        }
    }

    /// Build a JSON-RPC–style error body for 413 responses.
    fn payload_too_large_body() -> Bytes {
        Bytes::from(
            serde_json::json!({
                "error": {
                    "code": INVALID_REQUEST_CODE,
                    "message": format!("Request body exceeds maximum size of {} bytes", MAX_BODY_SIZE)
                }
            })
            .to_string(),
        )
    }

    async fn handle_request(
        self: Arc<Self>,
        req: Request<Incoming>,
    ) -> Result<
        Response<http_body_util::combinators::UnsyncBoxBody<Bytes, hyper::Error>>,
        hyper::Error,
    > {
        let start = Instant::now();

        if req.method() == hyper::Method::OPTIONS {
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header(
                    "Access-Control-Allow-Origin",
                    self.cors_allowed_origin.as_str(),
                )
                .header("Access-Control-Allow-Methods", "POST, OPTIONS")
                .header(
                    "Access-Control-Allow-Headers",
                    "Content-Type, Authorization, solana-client",
                )
                .header("Access-Control-Max-Age", "86400")
                .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                .unwrap());
        }

        // Shallow liveness check — verifies the gateway process is running.
        // Does not probe backend read/write nodes.
        if req.method() == hyper::Method::GET && req.uri().path() == "/health" {
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .header(
                    "Access-Control-Allow-Origin",
                    self.cors_allowed_origin.as_str(),
                )
                .body(
                    Full::new(Bytes::from(r#"{"status":"ok"}"#))
                        .map_err(|never| match never {})
                        .boxed_unsync(),
                )
                .unwrap());
        }

        if req.method() != hyper::Method::POST {
            Self::record_metrics(
                Some("method_not_allowed"),
                "unknown",
                "none",
                "405",
                start.elapsed().as_secs_f64(),
            );
            return Ok(self.error_response(StatusCode::METHOD_NOT_ALLOWED, None));
        }

        if let Some(content_length) = req.headers().get(hyper::header::CONTENT_LENGTH) {
            match content_length
                .to_str()
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
            {
                Some(len) if len > MAX_BODY_SIZE => {
                    warn!(
                        "Request body too large: Content-Length {} exceeds limit of {} bytes",
                        len, MAX_BODY_SIZE
                    );
                    Self::record_metrics(
                        Some("payload_too_large"),
                        "unknown",
                        "none",
                        "413",
                        start.elapsed().as_secs_f64(),
                    );
                    return Ok(self.error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        Some(Self::payload_too_large_body()),
                    ));
                }
                None => {
                    warn!("Unparseable Content-Length header: {:?}", content_length);
                }
                _ => {}
            }
        }

        let limited_body = Limited::new(req.into_body(), MAX_BODY_SIZE);
        let body_bytes = match limited_body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                if e.downcast_ref::<LengthLimitError>().is_some() {
                    warn!(
                        "Request body exceeded size limit of {} bytes",
                        MAX_BODY_SIZE
                    );
                    Self::record_metrics(
                        Some("payload_too_large"),
                        "unknown",
                        "none",
                        "413",
                        start.elapsed().as_secs_f64(),
                    );
                    return Ok(self.error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        Some(Self::payload_too_large_body()),
                    ));
                }
                warn!("Failed to read request body: {}", e);
                Self::record_metrics(
                    Some("bad_json"),
                    "unknown",
                    "none",
                    "400",
                    start.elapsed().as_secs_f64(),
                );
                return Ok(self.error_response(StatusCode::BAD_REQUEST, None));
            }
        };

        let json: Value = match serde_json::from_slice(&body_bytes) {
            Ok(json) => json,
            Err(e) => {
                warn!("Invalid JSON: {}", e);
                Self::record_metrics(
                    Some("bad_json"),
                    "unknown",
                    "none",
                    "400",
                    start.elapsed().as_secs_f64(),
                );
                return Ok(self.error_response(StatusCode::BAD_REQUEST, None));
            }
        };

        let method = match json.get("method").and_then(|m| m.as_str()) {
            Some(method) => method,
            None => {
                warn!("Missing or invalid 'method' field in JSON-RPC request");
                Self::record_metrics(
                    Some("invalid_method"),
                    "unknown",
                    "none",
                    "400",
                    start.elapsed().as_secs_f64(),
                );
                return Ok(self.error_response(StatusCode::BAD_REQUEST, None));
            }
        };

        let method_label = if KNOWN_RPC_METHODS.contains(&method) {
            method
        } else {
            "unknown"
        };

        let (target_url, target_label) = if method == "sendTransaction" {
            info!("Routing sendTransaction to write node");
            (&self.write_url, "write")
        } else {
            info!("Routing {} to read node", method);
            (&self.read_url, "read")
        };

        let uri = match target_url.parse::<hyper::Uri>() {
            Ok(uri) => uri,
            Err(e) => {
                error!("Invalid target URL {}: {}", target_url, e);
                Self::record_metrics(
                    Some("url_parse"),
                    method_label,
                    target_label,
                    "500",
                    start.elapsed().as_secs_f64(),
                );
                return Ok(self.error_response(StatusCode::INTERNAL_SERVER_ERROR, None));
            }
        };

        let forwarded_req = match Request::builder()
            .method(hyper::Method::POST)
            .uri(uri)
            .header("Content-Type", "application/json")
            .body(Full::new(body_bytes))
        {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to build forwarded request: {}", e);
                Self::record_metrics(
                    Some("request_build"),
                    method_label,
                    target_label,
                    "500",
                    start.elapsed().as_secs_f64(),
                );
                return Ok(self.error_response(StatusCode::INTERNAL_SERVER_ERROR, None));
            }
        };

        match self.client.request(forwarded_req).await {
            Ok(response) => {
                let status = response.status().as_u16().to_string();
                info!(
                    "Forwarded to {} - Status: {}",
                    target_url,
                    response.status()
                );
                Self::record_metrics(
                    None,
                    method_label,
                    target_label,
                    &status,
                    start.elapsed().as_secs_f64(),
                );

                let (mut parts, body) = response.into_parts();
                parts.headers.insert(
                    "Access-Control-Allow-Origin",
                    hyper::header::HeaderValue::from_str(&self.cors_allowed_origin).unwrap(),
                );
                parts.headers.insert(
                    "Access-Control-Allow-Methods",
                    hyper::header::HeaderValue::from_static("POST, OPTIONS"),
                );
                parts.headers.insert(
                    "Access-Control-Allow-Headers",
                    hyper::header::HeaderValue::from_static(
                        "Content-Type, Authorization, solana-client",
                    ),
                );
                Ok(Response::from_parts(parts, body.boxed_unsync()))
            }
            Err(e) => {
                error!("Failed to forward request to {}: {}", target_url, e);
                Self::record_metrics(
                    Some("backend_error"),
                    method_label,
                    target_label,
                    "502",
                    start.elapsed().as_secs_f64(),
                );
                Ok(self.error_response(StatusCode::BAD_GATEWAY, None))
            }
        }
    }
}

pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Contra Gateway");
    info!("  Port: {}", args.port);
    info!("  Write URL: {}", args.write_url);
    info!("  Read URL: {}", args.read_url);
    info!("  CORS Allowed Origin: {}", args.cors_allowed_origin);

    let gateway = Arc::new(Gateway::new(
        args.write_url,
        args.read_url,
        args.cors_allowed_origin,
    ));

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    let listener = TcpListener::bind(addr).await?;

    info!("Gateway listening on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let gateway = Arc::clone(&gateway);

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let gateway = Arc::clone(&gateway);
                async move { gateway.handle_request(req).await }
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                error!("Error serving connection: {:?}", err);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn start_test_gateway() -> SocketAddr {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .ok();

        let gateway = Arc::new(Gateway::new(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:1".to_string(),
            "*".to_string(),
        ));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = TokioIo::new(stream);
                let gateway = Arc::clone(&gateway);

                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let gateway = Arc::clone(&gateway);
                        async move { gateway.handle_request(req).await }
                    });
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
            }
        });

        addr
    }

    /// Send raw bytes to the test gateway and return the response as a string.
    async fn send_raw(addr: SocketAddr, data: &[u8]) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(data).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    /// Assert the response status line contains the expected HTTP status code.
    fn assert_status(response: &str, expected: u16) {
        let status_line = response.split("\r\n").next().unwrap_or("");
        let code = expected.to_string();
        assert!(
            status_line.contains(&code),
            "Expected {expected} in status line, got: {status_line}"
        );
    }

    #[tokio::test]
    async fn rejects_content_length_over_64kb() {
        let addr = start_test_gateway().await;
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            65 * 1024
        );
        let response = send_raw(addr, req.as_bytes()).await;
        assert_status(&response, 413);
    }

    #[tokio::test]
    async fn rejects_oversized_body_without_content_length() {
        let addr = start_test_gateway().await;

        // Build a chunked request with >64KB of data (no Content-Length header)
        let chunk_size = 65 * 1024;
        let mut raw = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            chunk_size
        )
        .into_bytes();
        raw.extend(vec![b'A'; chunk_size]);
        raw.extend_from_slice(b"\r\n0\r\n\r\n");

        let response = send_raw(addr, &raw).await;
        assert_status(&response, 413);
    }

    #[tokio::test]
    async fn accepts_body_at_exactly_64kb() {
        let addr = start_test_gateway().await;

        // Send exactly MAX_BODY_SIZE bytes — must NOT be rejected as 413
        let body = vec![b'A'; MAX_BODY_SIZE];
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len(),
        );
        let mut raw = req.into_bytes();
        raw.extend_from_slice(&body);

        let response = send_raw(addr, &raw).await;
        let status_line = response.split("\r\n").next().unwrap_or("");
        assert!(
            !status_line.contains("413"),
            "Body at exactly 64KB must not be rejected as too large, got: {}",
            status_line
        );
    }

    #[tokio::test]
    async fn rejects_oversized_body_despite_small_content_length() {
        let addr = start_test_gateway().await;

        // Lie: claim Content-Length: 100 but send 65KB of data
        let oversized = vec![b'A'; 65 * 1024];
        let header = "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\n";
        let mut raw = header.as_bytes().to_vec();
        raw.extend_from_slice(&oversized);

        let response = send_raw(addr, &raw).await;
        let status_line = response.split("\r\n").next().unwrap_or("");
        assert!(
            status_line.contains("413") || status_line.contains("400"),
            "Lying Content-Length with oversized body should be rejected, got: {}",
            status_line
        );
    }

    #[tokio::test]
    async fn accepts_normal_sized_request() {
        let addr = start_test_gateway().await;
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"getSlot"}"#;
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_raw(addr, req.as_bytes()).await;
        assert_status(&response, 502);
    }
}
