//! Integration tests for the auth service against a real Postgres via testcontainers.
//!
//! Covers: register, login, wallet challenge, wallet verification (including replay protection),
//! and wallet listing.
//!
//! Run with: `cd auth && cargo test --test integration -- --test-threads=1`

use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::Client;
use serde_json::{json, Value};
use solana_sdk::{signature::Signer, signer::keypair::Keypair};
use sqlx::postgres::PgPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::net::TcpListener;
use uuid::Uuid;

use contra_auth::{build_app, db, jwt::JwtConfig, AppState};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Spin up a Postgres container and return the connection URL.
/// The container is returned to keep it alive for the duration of the test.
async fn start_postgres() -> (String, testcontainers::ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_db_name("auth_test")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("failed to start postgres container");

    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db_url = format!("postgres://postgres:password@{}:{}/auth_test", host, port);

    (db_url, container)
}

/// Start the auth Axum app on a random port and return its address.
async fn start_app(db_url: &str) -> SocketAddr {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await
        .expect("failed to connect to test db");

    db::init_schema(&pool).await.expect("failed to init schema");

    let state = AppState {
        pool,
        jwt: Arc::new(JwtConfig::new("test-secret")),
    };

    let app = build_app(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

fn base_url(addr: SocketAddr) -> String {
    format!("http://{}", addr)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn test_register() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    let res = client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["username"], "alice");
    assert!(
        body["password_hash"].is_null(),
        "password_hash must not be exposed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_duplicate() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    let payload = json!({ "username": "alice", "password": "password123" });
    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&payload)
        .send()
        .await
        .unwrap();

    let res = client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 409);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_login_success() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let res = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert!(body["token"].is_string(), "expected a JWT token");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_login_wrong_password() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let res = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "wrongpassword" }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_challenge_requires_auth() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    let res = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_wallet_full_flow() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    // Register and login
    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();

    // Get challenge
    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let message = challenge_res["message"].as_str().unwrap();
    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();

    // Sign the challenge message with a Solana keypair
    let keypair = Keypair::new();
    let signature = keypair.sign_message(message.as_bytes());

    // Verify wallet
    let verify_res = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&json!({
            "pubkey": keypair.pubkey().to_string(),
            "nonce": nonce,
            "signature": signature.to_string(),
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(verify_res.status(), 200);
    let body: Value = verify_res.json().await.unwrap();
    assert_eq!(body["pubkey"], keypair.pubkey().to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_wallet_replay_rejected() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();

    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let message = challenge_res["message"].as_str().unwrap();
    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();

    let keypair = Keypair::new();
    let signature = keypair.sign_message(message.as_bytes());

    let payload = json!({
        "pubkey": keypair.pubkey().to_string(),
        "nonce": nonce,
        "signature": signature.to_string(),
    });

    // First verify — should succeed
    let first = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);

    // Replay — same nonce must be rejected
    let second = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_wallet_invalid_pubkey() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();

    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();
    let keypair = Keypair::new();
    let signature = keypair.sign_message(challenge_res["message"].as_str().unwrap().as_bytes());

    let res = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&json!({
            "pubkey": "not-a-valid-pubkey",
            "nonce": nonce,
            "signature": signature.to_string(),
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_wallet_invalid_signature_format() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();

    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();
    let keypair = Keypair::new();

    let res = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&json!({
            "pubkey": keypair.pubkey().to_string(),
            "nonce": nonce,
            "signature": "not-a-valid-signature",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_wallet_wrong_signature() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();

    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let message = challenge_res["message"].as_str().unwrap();
    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();

    let keypair = Keypair::new();
    // Sign with a different keypair — signature won't verify against the pubkey we submit.
    let wrong_keypair = Keypair::new();
    let signature = wrong_keypair.sign_message(message.as_bytes());

    let res = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&json!({
            "pubkey": keypair.pubkey().to_string(),
            "nonce": nonce,
            "signature": signature.to_string(),
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_wallet_duplicate() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();
    let keypair = Keypair::new();

    // Helper: get a fresh challenge and verify the same wallet.
    let do_verify = |_token: &str, nonce: Uuid, message: &str| {
        let signature = keypair.sign_message(message.as_bytes());
        (json!({
            "pubkey": keypair.pubkey().to_string(),
            "nonce": nonce,
            "signature": signature.to_string(),
        }),)
    };

    // First verification — must succeed.
    let challenge1: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let (payload1,) = do_verify(
        token,
        challenge1["nonce"].as_str().unwrap().parse().unwrap(),
        challenge1["message"].as_str().unwrap(),
    );

    let first = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&payload1)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);

    // Second verification of the same wallet with a fresh nonce — must conflict.
    let challenge2: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let (payload2,) = do_verify(
        token,
        challenge2["nonce"].as_str().unwrap().parse().unwrap(),
        challenge2["message"].as_str().unwrap(),
    );

    let second = client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&payload2)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_wallets() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "alice", "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap();

    // No wallets yet
    let wallets: Value = client
        .get(format!("{}/auth/wallets", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(wallets.as_array().unwrap().len(), 0);

    // Verify a wallet
    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let message = challenge_res["message"].as_str().unwrap();
    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();
    let keypair = Keypair::new();
    let signature = keypair.sign_message(message.as_bytes());

    client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(token)
        .json(&json!({
            "pubkey": keypair.pubkey().to_string(),
            "nonce": nonce,
            "signature": signature.to_string(),
        }))
        .send()
        .await
        .unwrap();

    // Now wallets should have one entry
    let wallets: Value = client
        .get(format!("{}/auth/wallets", base_url(addr)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(wallets.as_array().unwrap().len(), 1);
    assert_eq!(wallets[0]["pubkey"], keypair.pubkey().to_string());
}
