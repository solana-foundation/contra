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
        pool_status: contra_auth::pool_status::PoolStatus::new_healthy(),
    };

    let app = build_app(state, "*");

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
async fn test_register_username_too_short() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    for username in ["", "ab", "abcd"] {
        let res = client
            .post(format!("{}/auth/register", base_url(addr)))
            .json(&json!({ "username": username, "password": "password123" }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            400,
            "expected 400 for username {:?}",
            username
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_username_too_long() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    let username = "a".repeat(33);
    let res = client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": username, "password": "password123" }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_username_invalid_chars() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    for username in ["alice bob", "alice@bob", "alice!"] {
        let res = client
            .post(format!("{}/auth/register", base_url(addr)))
            .json(&json!({ "username": username, "password": "password123" }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            400,
            "expected 400 for username {:?}",
            username
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_username_valid_formats() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    // Underscores, hyphens, and mixed case should all be accepted.
    for username in ["alice", "alice_bob", "alice-bob", "Alice123"] {
        let res = client
            .post(format!("{}/auth/register", base_url(addr)))
            .json(&json!({ "username": username, "password": "password123" }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            200,
            "expected 200 for username {:?}",
            username
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_password_too_short() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    // Empty password and passwords shorter than 6 characters must both be rejected.
    for password in ["", "abc", "12345"] {
        let res = client
            .post(format!("{}/auth/register", base_url(addr)))
            .json(&json!({ "username": "alice", "password": password }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            400,
            "expected 400 for password {:?}",
            password
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_password_too_long() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    // 129 characters — one over the max allowed.
    let password = "a".repeat(129);

    let res = client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": "alice", "password": password }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_register_password_at_boundaries() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    // Exactly 6 characters (min) and exactly 128 characters (max) must both succeed.
    for (username, password) in [("min_user", "a".repeat(6)), ("max_user", "a".repeat(128))] {
        let res = client
            .post(format!("{}/auth/register", base_url(addr)))
            .json(&json!({ "username": username, "password": password }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            200,
            "expected 200 for password of length {}",
            password.len()
        );
    }
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
async fn test_login_unknown_username() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;

    let res = Client::new()
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": "doesnotexist", "password": "password123" }))
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

/// Fires N concurrent registration requests for the same username and asserts that:
///   - exactly one succeeds (200 OK), and
///   - every other response is 409 Conflict — never 500.
///
/// This is the actual TOCTOU scenario. The old check-then-insert pattern let multiple
/// requests pass the SELECT guard simultaneously, causing the losing INSERT to hit the
/// UNIQUE constraint and return 500. The fix drops the pre-check and catches the
/// constraint violation at the INSERT site, so all losers get 409 regardless of timing.
#[tokio::test(flavor = "multi_thread")]
async fn test_register_concurrent_same_username_returns_409_not_500() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;

    let url = format!("{}/auth/register", base_url(addr));
    let payload = json!({ "username": "raceuser", "password": "password123" });

    // Spawn 10 requests simultaneously. We want genuine concurrency so we collect
    // the futures first and then await them all at once via join_all.
    const N: usize = 10;
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let url = url.clone();
            let payload = payload.clone();
            tokio::spawn(async move {
                Client::new()
                    .post(&url)
                    .json(&payload)
                    .send()
                    .await
                    .expect("request failed")
                    .status()
                    .as_u16()
            })
        })
        .collect();

    let statuses: Vec<u16> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("task panicked"))
        .collect();

    let successes = statuses.iter().filter(|&&s| s == 200).count();
    let conflicts = statuses.iter().filter(|&&s| s == 409).count();
    let errors = statuses.iter().filter(|&&s| s == 500).count();

    assert_eq!(successes, 1, "exactly one registration should succeed");
    assert_eq!(
        conflicts,
        N - 1,
        "all other requests should get 409, not 500"
    );
    assert_eq!(errors, 0, "no request should return 500");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cleanup_stale_challenges() {
    let (db_url, _container) = start_postgres().await;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to test db");

    db::init_schema(&pool).await.expect("failed to init schema");

    // Insert a user so we have a valid user_id to reference.
    let user = db::insert_user(&pool, "cleanupuser", "fakehash")
        .await
        .expect("failed to insert user");

    // Insert an expired challenge (expires_at in the past, not used).
    sqlx::query(
        r#"
        INSERT INTO contra_auth.challenges (id, user_id, nonce, expires_at)
        VALUES ($1, $2, $3, NOW() - INTERVAL '1 hour')
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .expect("failed to insert expired challenge");

    // Insert a used challenge (used_at is set, not yet expired).
    sqlx::query(
        r#"
        INSERT INTO contra_auth.challenges (id, user_id, nonce, expires_at, used_at)
        VALUES ($1, $2, $3, NOW() + INTERVAL '5 minutes', NOW())
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .expect("failed to insert used challenge");

    // Insert a valid challenge (not expired, not used) — must survive cleanup.
    let valid_nonce = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO contra_auth.challenges (id, user_id, nonce, expires_at)
        VALUES ($1, $2, $3, NOW() + INTERVAL '10 minutes')
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(valid_nonce)
    .execute(&pool)
    .await
    .expect("failed to insert valid challenge");

    let deleted = db::cleanup_stale_challenges(&pool)
        .await
        .expect("cleanup failed");

    assert_eq!(deleted, 2, "expired and used challenges should be removed");

    let remaining: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM contra_auth.challenges WHERE nonce = $1")
            .bind(valid_nonce)
            .fetch_one(&pool)
            .await
            .expect("count query failed");

    assert_eq!(remaining.0, 1, "valid challenge must not be deleted");
}

/// Helper: register a user, log in, verify a wallet, and return the token and pubkey.
async fn setup_user_with_wallet(addr: SocketAddr, username: &str) -> (String, String) {
    let client = Client::new();

    client
        .post(format!("{}/auth/register", base_url(addr)))
        .json(&json!({ "username": username, "password": "password123" }))
        .send()
        .await
        .unwrap();

    let login_res: Value = client
        .post(format!("{}/auth/login", base_url(addr)))
        .json(&json!({ "username": username, "password": "password123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let token = login_res["token"].as_str().unwrap().to_string();

    let challenge_res: Value = client
        .post(format!("{}/auth/challenge-wallet", base_url(addr)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let message = challenge_res["message"].as_str().unwrap().to_string();
    let nonce: Uuid = challenge_res["nonce"].as_str().unwrap().parse().unwrap();
    let keypair = Keypair::new();
    let signature = keypair.sign_message(message.as_bytes());

    client
        .post(format!("{}/auth/verify-wallet", base_url(addr)))
        .bearer_auth(&token)
        .json(&json!({
            "pubkey": keypair.pubkey().to_string(),
            "nonce": nonce,
            "signature": signature.to_string(),
        }))
        .send()
        .await
        .unwrap();

    (token, keypair.pubkey().to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_delete_wallet_success() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    let (token, pubkey) = setup_user_with_wallet(addr, "alice").await;

    // Wallet should be present before deletion.
    let wallets: Value = client
        .get(format!("{}/auth/wallets", base_url(addr)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(wallets.as_array().unwrap().len(), 1);

    // Delete the wallet.
    let res = client
        .delete(format!("{}/auth/wallets/{}", base_url(addr), pubkey))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    // Wallet should no longer appear in the list.
    let wallets: Value = client
        .get(format!("{}/auth/wallets", base_url(addr)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(wallets.as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_delete_wallet_not_found() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;

    let (token, _) = setup_user_with_wallet(addr, "alice").await;

    let res = Client::new()
        .delete(format!(
            "{}/auth/wallets/{}",
            base_url(addr),
            "nonexistentpubkey"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_delete_wallet_cannot_delete_other_users_wallet() {
    let (db_url, _container) = start_postgres().await;
    let addr = start_app(&db_url).await;
    let client = Client::new();

    let (_, pubkey) = setup_user_with_wallet(addr, "alice").await;
    let (bob_token, _) = setup_user_with_wallet(addr, "bobob").await;

    // Bob tries to delete Alice's wallet — must not succeed.
    let res = client
        .delete(format!("{}/auth/wallets/{}", base_url(addr), pubkey))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 400);
}
