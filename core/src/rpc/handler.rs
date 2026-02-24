use {
    super::{api::ContraRpcServer, rpc_impl::ContraRpcImpl},
    crate::rpc::{
        error::{INTERNAL_ERROR_CODE, PARSE_ERROR_CODE},
        rpc_impl::{ReadDeps, WriteDeps},
    },
    http_body_util::{BodyExt, Full},
    hyper::{body::Bytes, Method, Request, Response, StatusCode},
    jsonrpsee::server::RpcModule,
    std::sync::Arc,
};

pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    rpc_module: Arc<RpcModule<()>>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
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
                            .body(Full::new(Bytes::from(
                                format!(r#"{{"status":"ok","slot":{}}}"#, slot),
                            )))
                            .unwrap(),
                        None => {
                            tracing::warn!("Health check: getSlot returned unexpected response: {}", resp);
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
            let body_bytes = req.collect().await?.to_bytes();

            // Process the JSON-RPC request using jsonrpsee
            // Convert body_bytes to a String first
            let json_str = String::from_utf8(body_bytes.to_vec())
                .unwrap_or_else(|_| format!(r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Parse error: Invalid UTF-8"}},"id":null}}"#, PARSE_ERROR_CODE));

            // The second parameter is the maximum response size (10MB)
            let max_response_size = 10 * 1024 * 1024;
            let response = rpc_module
                .raw_json_request(&json_str, max_response_size)
                .await;

            // The response is a Result<(String, _), _>, we need to handle it
            let json_response = match response {
                Ok((response_str, _)) => response_str,
                Err(e) => {
                    // Create a JSON-RPC error response
                    format!(
                        r#"{{"jsonrpc":"2.0","error":{{"code":{},"message":"Internal error: {}"}},"id":null}}"#,
                        INTERNAL_ERROR_CODE, e
                    )
                }
            };

            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(json_response)))
                .unwrap()
        }
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap(),
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
