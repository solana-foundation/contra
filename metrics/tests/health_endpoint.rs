//! Integration test for the /health endpoint exposed by start_metrics_server_with_health.
//!
//! Spins up the real axum server on an ephemeral port and verifies the HTTP
//! response codes and bodies for each HealthOutcome variant.

use contra_metrics::{HealthConfig, HealthState};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

async fn boot(health: Arc<HealthState>) -> u16 {
    // Bind here, hand the listener to the server — no drop-and-rebind window for
    // another process or parallel test to steal the port.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    contra_metrics::start_metrics_server_with_health_from_listener(listener, health);
    // Give axum a moment to start serving. The server spawns on a tokio task so this
    // is the simplest way to await readiness without polling.
    tokio::time::sleep(Duration::from_millis(100)).await;
    port
}

#[tokio::test]
async fn health_returns_200_when_healthy() {
    let h = HealthState::new(HealthConfig::operator());
    let port = boot(h).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/health", port))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains(r#""status":"ok""#), "body was: {}", body);
}

#[tokio::test]
async fn health_returns_503_when_backlog_exceeds_ceiling() {
    let cfg = HealthConfig {
        max_pending: 5,
        stale_threshold_secs: 30,
        require_continuous_progress: false,
    };
    let h = HealthState::new(cfg);
    h.set_pending(10);
    let port = boot(h).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/health", port))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    let body = resp.text().await.unwrap();
    assert!(body.contains(r#""reason":"backlog""#), "body was: {}", body);
    assert!(body.contains(r#""pending":10"#));
    assert!(body.contains(r#""ceiling":5"#));
}

#[tokio::test]
async fn health_returns_503_when_stalled() {
    // Force a stalled state by setting last_progress_at to a long time ago,
    // pending > 0, and the staleness threshold exceeded.
    let cfg = HealthConfig {
        max_pending: 100,
        stale_threshold_secs: 1,
        require_continuous_progress: false,
    };
    let h = HealthState::new(cfg);
    h.set_pending(3);
    // Set last_progress_at to roughly 2 hours ago in unix seconds.
    let two_hours_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 7200;
    h.last_progress_at().store(two_hours_ago, Ordering::Relaxed);

    let port = boot(h).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/health", port))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    let body = resp.text().await.unwrap();
    assert!(body.contains(r#""reason":"stalled""#), "body was: {}", body);
    assert!(body.contains(r#""pending":3"#));
}

#[tokio::test]
async fn metrics_endpoint_still_works_alongside_health() {
    let h = HealthState::new(HealthConfig::operator());
    let port = boot(h).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/metrics", port))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp.headers().get("content-type").unwrap();
    assert!(ct.to_str().unwrap().starts_with("text/plain"));
}
