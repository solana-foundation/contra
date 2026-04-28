use {
    super::handler::{create_rpc_module, handle_request},
    crate::{
        health::HeartbeatRegistry,
        nodes::node::WorkerHandle,
        rpc::rpc_impl::{ReadDeps, WriteDeps},
    },
    hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder,
    },
    std::{net::SocketAddr, sync::Arc},
    tokio::{net::TcpListener, sync::Semaphore},
    tokio_util::sync::CancellationToken,
    tracing::{debug, error, info, warn},
};

/// Configuration for the RPC service
pub struct RpcServiceConfig {
    pub port: u16,
    pub max_connections: usize,
    pub read_deps: Option<ReadDeps>,
    pub write_deps: Option<WriteDeps>,
    pub heartbeats: HeartbeatRegistry,
    pub shutdown_token: CancellationToken,
}

pub async fn start_rpc_service(
    config: RpcServiceConfig,
) -> Result<WorkerHandle, Box<dyn std::error::Error>> {
    let RpcServiceConfig {
        port,
        max_connections,
        read_deps,
        write_deps,
        heartbeats,
        shutdown_token,
    } = config;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    let enabled_ops = match (write_deps.is_some(), read_deps.is_some()) {
        (true, true) => "read+write",
        (true, false) => "write-only",
        (false, true) => "read-only",
        (false, false) => "none",
    };

    // Create the RPC module once, to be shared across all connections
    let rpc_module = Arc::new(create_rpc_module(read_deps, write_deps).await);
    let heartbeats = Arc::new(heartbeats);

    // Limit concurrent connections
    let max_connections = Arc::new(Semaphore::new(max_connections));
    info!(
        "RPC service listening on http://{} (max connections: {}, operations: {})",
        addr,
        max_connections.available_permits(),
        enabled_ops
    );

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Accept new connections
                result = listener.accept() => {
                    match result {
                        Ok((tcp_stream, peer_addr)) => {
                            // Try to acquire a connection permit
                            let permit = match max_connections.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    warn!("Max connections reached, rejecting connection from {}", peer_addr);
                                    continue;
                                }
                            };

                            let io = TokioIo::new(tcp_stream);
                            let rpc_module_clone = rpc_module.clone();
                            let heartbeats_clone = heartbeats.clone();

                            tokio::spawn(async move {
                                // Hold permit for connection lifetime
                                let _permit = permit;
                                debug!("Accepted connection from {}", peer_addr);

                                let service = hyper::service::service_fn(move |req| {
                                    handle_request(req, rpc_module_clone.clone(), heartbeats_clone.clone())
                                });

                                // Configure connection with timeouts
                                let result = Builder::new(TokioExecutor::new())
                                    .http1()
                                    .keep_alive(true)
                                    .serve_connection(io, service)
                                    .await;

                                match result {
                                    Ok(_) => debug!("Connection from {} closed normally", peer_addr),
                                    Err(err) => error!("Error serving connection from {}: {:?}", peer_addr, err),
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                            // Don't exit, just continue accepting connections
                        }
                    }
                }

                // Handle shutdown signal
                _ = shutdown_token.cancelled() => {
                    info!("RPC service received shutdown signal");
                    break;
                }
            }
        }

        info!("RPC service stopped");
    });

    Ok(WorkerHandle::new("RPC".to_string(), handle))
}
