pub use once_cell::sync::Lazy;
pub use prometheus;
pub use prometheus::{CounterVec, GaugeVec, HistogramVec};

#[macro_export]
macro_rules! counter_vec {
    ($name:ident, $metric_name:expr, $help:expr, $labels:expr) => {
        pub static $name: $crate::Lazy<$crate::CounterVec> = $crate::Lazy::new(|| {
            $crate::prometheus::register_counter_vec!($metric_name, $help, $labels).unwrap()
        });
    };
}

#[macro_export]
macro_rules! gauge_vec {
    ($name:ident, $metric_name:expr, $help:expr, $labels:expr) => {
        pub static $name: $crate::Lazy<$crate::GaugeVec> = $crate::Lazy::new(|| {
            $crate::prometheus::register_gauge_vec!($metric_name, $help, $labels).unwrap()
        });
    };
}

#[macro_export]
macro_rules! histogram_vec {
    ($name:ident, $metric_name:expr, $help:expr, $labels:expr) => {
        pub static $name: $crate::Lazy<$crate::HistogramVec> = $crate::Lazy::new(|| {
            $crate::prometheus::register_histogram_vec!($metric_name, $help, $labels).unwrap()
        });
    };
}

#[macro_export]
macro_rules! init_metrics {
    ($($metric:expr),* $(,)?) => {
        $($crate::Lazy::force(&$metric);)*
    };
}

pub trait MetricLabel {
    fn as_label(&self) -> &'static str;
}

pub fn start_metrics_server(port: u16) {
    use prometheus::{Encoder, TextEncoder};

    async fn metrics_handler() -> ([(axum::http::header::HeaderName, &'static str); 1], Vec<u8>) {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4",
            )],
            buffer,
        )
    }

    let app = axum::Router::new().route("/metrics", axum::routing::get(metrics_handler));

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Metrics server listening on {}", addr);

    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!("Metrics server error: {}", e);
                }
            }
            Err(e) => {
                tracing::error!("Failed to bind metrics server on {}: {}", addr, e);
            }
        }
    });
}
