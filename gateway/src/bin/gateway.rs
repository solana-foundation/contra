use clap::Parser;
use private_channel_gateway::{run, Args};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let metrics_port = std::env::var("METRICS_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(9101);
    private_channel_gateway::metrics::init();
    private_channel_metrics::start_metrics_server(metrics_port);

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install default crypto provider");

    let args = Args::parse();
    run(args).await
}
