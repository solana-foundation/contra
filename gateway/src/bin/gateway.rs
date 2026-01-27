use clap::Parser;
use contra_gateway::{run, Args};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install default crypto provider");

    let args = Args::parse();
    run(args).await
}
