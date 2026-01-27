use clap::Parser;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

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

    async fn handle_request(
        self: Arc<Self>,
        req: Request<Incoming>,
    ) -> Result<
        Response<http_body_util::combinators::UnsyncBoxBody<Bytes, hyper::Error>>,
        hyper::Error,
    > {
        // Handle CORS preflight requests
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

        // Only accept POST requests
        if req.method() != hyper::Method::POST {
            return Ok(Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(
                    "Access-Control-Allow-Origin",
                    self.cors_allowed_origin.as_str(),
                )
                .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                .unwrap());
        }

        // Read the request body to inspect the method for routing
        let body_bytes = match req.into_body().collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                warn!("Failed to read request body: {}", e);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(
                        "Access-Control-Allow-Origin",
                        self.cors_allowed_origin.as_str(),
                    )
                    .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                    .unwrap());
            }
        };

        // Parse as JSON to determine routing
        let json: Value = match serde_json::from_slice(&body_bytes) {
            Ok(json) => json,
            Err(e) => {
                warn!("Invalid JSON: {}", e);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(
                        "Access-Control-Allow-Origin",
                        self.cors_allowed_origin.as_str(),
                    )
                    .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                    .unwrap());
            }
        };

        // Validate JSON-RPC structure and get method
        let method = match json.get("method").and_then(|m| m.as_str()) {
            Some(method) => method,
            None => {
                warn!("Missing or invalid 'method' field in JSON-RPC request");
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(
                        "Access-Control-Allow-Origin",
                        self.cors_allowed_origin.as_str(),
                    )
                    .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                    .unwrap());
            }
        };

        // Route based on method
        let target_url = if method == "sendTransaction" {
            info!("Routing sendTransaction to write node");
            &self.write_url
        } else {
            info!("Routing {} to read node", method);
            &self.read_url
        };

        // Parse target URL
        let uri = match target_url.parse::<hyper::Uri>() {
            Ok(uri) => uri,
            Err(e) => {
                error!("Invalid target URL {}: {}", target_url, e);
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(
                        "Access-Control-Allow-Origin",
                        self.cors_allowed_origin.as_str(),
                    )
                    .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                    .unwrap());
            }
        };

        // Build forwarded request
        let forwarded_req = match Request::builder()
            .method(hyper::Method::POST)
            .uri(uri)
            .header("Content-Type", "application/json")
            .body(Full::new(body_bytes))
        {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to build forwarded request: {}", e);
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(
                        "Access-Control-Allow-Origin",
                        self.cors_allowed_origin.as_str(),
                    )
                    .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                    .unwrap());
            }
        };

        // Forward the request and stream the response directly (no buffering!)
        match self.client.request(forwarded_req).await {
            Ok(response) => {
                info!(
                    "Forwarded to {} - Status: {}",
                    target_url,
                    response.status()
                );
                // Response body is streamed directly without reading into memory
                let (mut parts, body) = response.into_parts();
                // Add CORS headers to the response
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
                Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .header(
                        "Access-Control-Allow-Origin",
                        self.cors_allowed_origin.as_str(),
                    )
                    .body(Empty::new().map_err(|never| match never {}).boxed_unsync())
                    .unwrap())
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
