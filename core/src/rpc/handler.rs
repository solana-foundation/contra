use {
    super::{api::ContraRpcServer, rpc_impl::ContraRpcImpl},
    crate::rpc::{
        constants::{MAX_BODY_SIZE, MAX_RESPONSE_SIZE},
        error::{INTERNAL_ERROR_CODE, PARSE_ERROR_CODE},
        rpc_impl::{ReadDeps, WriteDeps},
    },
    http_body_util::{BodyExt, Full, LengthLimitError, Limited},
    hyper::{body::Bytes, Method, Request, Response, StatusCode},
    jsonrpsee::server::RpcModule,
    std::sync::Arc,
    tracing::warn,
};

pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    rpc_module: Arc<RpcModule<()>>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let payload_too_large = || {
        let body = format!(
            r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Request body exceeds maximum size of {} bytes"}},"id":null}}"#,
            PARSE_ERROR_CODE, MAX_BODY_SIZE
        );
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .expect("static response builder")
    };

    let response = match (req.method(), req.uri().path()) {
        (&Method::GET, "/health") => {
            // Health check endpoint for monitoring and load balancers.
            // Returns 200 with slot when the node is responsive, 503 otherwise.
            let slot_request = r#"{"jsonrpc":"2.0","id":1,"method":"getSlot"}"#;
            match rpc_module.raw_json_request(slot_request, 1024).await {
                Ok((resp, _)) => {
                    match serde_json::from_str::<serde_json::Value>(&resp)
                        .ok()
                        .and_then(|v| v.get("result").and_then(|r| r.as_u64()))
                    {
                        Some(slot) => Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", "application/json")
                            .body(Full::new(Bytes::from(format!(
                                r#"{{"status":"ok","slot":{}}}"#,
                                slot
                            ))))
                            .unwrap(),
                        None => {
                            tracing::warn!(
                                "Health check: getSlot returned unexpected response: {}",
                                resp
                            );
                            Response::builder()
                                .status(StatusCode::SERVICE_UNAVAILABLE)
                                .header("Content-Type", "application/json")
                                .body(Full::new(Bytes::from(
                                    r#"{"status":"degraded","error":"unexpected getSlot response"}"#,
                                )))
                                .unwrap()
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Health check: getSlot RPC call failed: {}", e);
                    Response::builder()
                        .status(StatusCode::SERVICE_UNAVAILABLE)
                        .header("Content-Type", "application/json")
                        .body(Full::new(Bytes::from(
                            r#"{"status":"error","error":"RPC unavailable"}"#,
                        )))
                        .unwrap()
                }
            }
        }
        (&Method::POST, "/") => {
            // Early reject via Content-Length if present
            if let Some(cl) = req.headers().get(hyper::header::CONTENT_LENGTH) {
                match cl.to_str().ok().and_then(|s| s.parse::<usize>().ok()) {
                    Some(len) if len > MAX_BODY_SIZE => {
                        warn!(
                            "Request body too large: {} bytes (limit: {})",
                            len, MAX_BODY_SIZE
                        );
                        return Ok(payload_too_large());
                    }
                    None => {
                        warn!(
                            "Rejecting request with unparseable Content-Length header: {:?}",
                            cl
                        );
                        return Ok(Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header("Content-Type", "application/json")
                            .body(Full::new(Bytes::from(
                                format!(
                                    r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Invalid Content-Length header"}},"id":null}}"#,
                                    PARSE_ERROR_CODE
                                ),
                            )))
                            .expect("static response builder"));
                    }
                    _ => {}
                }
            }

            let body_bytes = match Limited::new(req.into_body(), MAX_BODY_SIZE).collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(e) => {
                    if e.downcast_ref::<LengthLimitError>().is_some() {
                        warn!(
                            "Request body exceeded size limit of {} bytes",
                            MAX_BODY_SIZE
                        );
                        return Ok(payload_too_large());
                    }
                    warn!("Failed to read request body: {}", e);
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header("Content-Type", "application/json")
                        .body(Full::new(Bytes::from(
                            format!(r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Failed to read request body"}},"id":null}}"#, PARSE_ERROR_CODE),
                        )))
                        .expect("static response builder"));
                }
            };

            // Validate UTF-8 before passing to jsonrpsee
            let json_str = match String::from_utf8(body_bytes.to_vec()) {
                Ok(s) => s,
                Err(_) => {
                    warn!("Received non-UTF-8 request body");
                    return Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/json")
                        .body(Full::new(Bytes::from(
                            format!(r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Parse error: Invalid UTF-8"}},"id":null}}"#, PARSE_ERROR_CODE),
                        )))
                        .expect("static response builder"));
                }
            };

            // Process the JSON-RPC request using jsonrpsee
            let json_response = match rpc_module
                .raw_json_request(&json_str, MAX_RESPONSE_SIZE)
                .await
            {
                Ok((response_str, _)) => response_str,
                Err(e) => {
                    warn!("JSON-RPC processing failed: {}", e);
                    format!(
                        r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Internal error"}},"id":null}}"#,
                        INTERNAL_ERROR_CODE
                    )
                }
            };

            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(json_response)))
                .expect("static response builder")
        }
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .expect("static response builder"),
    };

    Ok(response)
}

pub async fn create_rpc_module(
    read_deps: Option<ReadDeps>,
    write_deps: Option<WriteDeps>,
) -> RpcModule<()> {
    let rpc_impl = ContraRpcImpl::new(read_deps, write_deps).await;
    let mut module = RpcModule::new(());

    // Register all RPC methods
    module
        .merge(rpc_impl.into_rpc())
        .expect("Failed to register RPC methods");

    module
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn start_test_rpc_server() -> std::net::SocketAddr {
        // Empty RPC module — sufficient for testing body validation layer
        let rpc_module = Arc::new(RpcModule::new(()));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
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

    /// Send raw bytes to the test server and return the response as a string.
    async fn send_raw(addr: std::net::SocketAddr, data: &[u8]) -> String {
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
        let addr = start_test_rpc_server().await;
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            65 * 1024
        );
        let response = send_raw(addr, req.as_bytes()).await;
        assert_status(&response, 413);
        assert!(
            response.contains(&PARSE_ERROR_CODE.to_string()),
            "Expected PARSE_ERROR_CODE in body"
        );
    }

    #[tokio::test]
    async fn rejects_oversized_body_without_content_length() {
        let addr = start_test_rpc_server().await;

        // Chunked request with >64KB of data (no Content-Length header)
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
        let addr = start_test_rpc_server().await;

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
    async fn lying_content_length_does_not_oom() {
        let addr = start_test_rpc_server().await;

        // Lie: claim Content-Length: 100 but send 65KB of data.
        // hyper respects the Content-Length header and reads only 100 bytes,
        // so the handler sees a small body and processes it normally (200).
        // The key invariant: the server does NOT read unbounded data.
        let oversized = vec![b'A'; 65 * 1024];
        let header = "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\n";
        let mut raw = header.as_bytes().to_vec();
        raw.extend_from_slice(&oversized);

        let response = send_raw(addr, &raw).await;
        let status_line = response.split("\r\n").next().unwrap_or("");
        assert!(
            !status_line.contains("413"),
            "hyper reads only Content-Length bytes; should not trigger size limit, got: {}",
            status_line
        );
    }

    #[tokio::test]
    async fn accepts_normal_json_rpc_request() {
        let addr = start_test_rpc_server().await;
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"getSlot"}"#;
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_raw(addr, req.as_bytes()).await;
        // Empty RPC module returns method-not-found, but status is 200 (JSON-RPC errors are in-band)
        assert_status(&response, 200);
        assert!(
            response.contains("jsonrpc"),
            "Expected JSON-RPC response body"
        );
    }

    #[tokio::test]
    async fn returns_404_for_non_root_path() {
        let addr = start_test_rpc_server().await;
        let req = "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let response = send_raw(addr, req.as_bytes()).await;
        assert_status(&response, 404);
    }

    #[tokio::test]
    async fn rejects_non_utf8_body_with_parse_error() {
        let addr = start_test_rpc_server().await;

        // Send invalid UTF-8 bytes as the body
        let invalid_utf8: &[u8] = &[0xFF, 0xFE, 0x00, 0x01];
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            invalid_utf8.len(),
        );
        let mut raw = req.into_bytes();
        raw.extend_from_slice(invalid_utf8);

        let response = send_raw(addr, &raw).await;
        assert_status(&response, 200); // JSON-RPC errors are in-band
        assert!(
            response.contains(&PARSE_ERROR_CODE.to_string()),
            "Expected PARSE_ERROR_CODE in body"
        );
        assert!(
            response.contains("Invalid UTF-8"),
            "Expected UTF-8 error message"
        );
    }
}
