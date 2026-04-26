//! End-to-end coverage for `try_jit_mint_initialization`
//! (`indexer/src/operator/sender/mint.rs`) via the
//! `test_hooks::jit_mint_init` wrapper.
//!
//! Drives the production helper against a scripted `MockRpcServer`, so the
//! full code path — on-chain probe, `InitializeMint` send, confirmation
//! poll, and `mint_is_initialized_on_chain` backoff — is exercised
//! end-to-end rather than replayed by hand at the wire layer.

#[path = "sender_fixtures.rs"]
mod sender_fixtures;

use {
    base64::{engine::general_purpose::STANDARD, Engine as _},
    contra_indexer::{
        config::ProgramType,
        operator::{
            sender::{test_hooks, types::InstructionWithSigners},
            utils::instruction_util::MintToBuilder,
            SignerUtil,
        },
        storage::{
            common::{models::DbMint, storage::mock::MockStorage},
            Storage,
        },
    },
    sender_fixtures::{
        blockhash_reply, confirmed_status_reply, ensure_admin_signer_env, make_config,
        null_status_reply, send_transaction_echo_reply,
    },
    serde_json::json,
    solana_keychain::SolanaSigner,
    solana_sdk::{commitment_config::CommitmentLevel, pubkey::Pubkey},
    spl_token::{
        solana_program::{program_option::COption, program_pack::Pack},
        state::Mint,
    },
    std::sync::Arc,
    test_utils::mock_rpc::{MockRpcServer, Reply},
};

/// Pack an SPL `Mint` into its on-chain bytes so the byte layout the
/// production helper decodes in `is_initialized_mint_data` matches what
/// a real Solana validator would return for `getAccountInfo`.
fn pack_mint_bytes(is_initialized: bool) -> Vec<u8> {
    let mint = Mint {
        mint_authority: COption::Some(Pubkey::new_unique()),
        supply: 0,
        decimals: 6,
        is_initialized,
        freeze_authority: COption::None,
    };
    let mut data = vec![0u8; Mint::LEN];
    Mint::pack(mint, &mut data).expect("pack mint");
    data
}

/// Build a `getAccountInfo` success-shaped reply carrying the given
/// account bytes in base64 encoding (Solana JSON-RPC wire shape).
fn account_info_reply(data: &[u8]) -> Reply {
    Reply::result(json!({
        "context": { "slot": 100 },
        "value": {
            "data": [STANDARD.encode(data), "base64"],
            "executable": false,
            "lamports": 1_461_600u64,
            "owner": spl_token::id().to_string(),
            "rentEpoch": 0u64,
            "space": data.len(),
        }
    }))
}

/// Build a minimal `InstructionWithSigners` to hand into the JIT hook.
/// The helper passes this through verbatim on success and never inspects
/// its contents, so an empty instruction list with the admin fee-payer is
/// sufficient.
fn make_passthrough_instruction() -> InstructionWithSigners {
    let admin = SignerUtil::admin_signer();
    InstructionWithSigners {
        instructions: vec![],
        fee_payer: admin.pubkey(),
        signers: vec![admin],
        compute_unit_price: None,
        compute_budget: None,
    }
}

/// Build a `MintToBuilder` with the minimum field set
/// `try_jit_mint_initialization` reads (`get_mint`, plus enough to flow
/// through `handle_transaction_builder` if scenarios 2–6 reach the
/// InitializeMint build path).
fn make_mint_builder(mint: Pubkey) -> MintToBuilder {
    let mut builder = MintToBuilder::new();
    let admin = SignerUtil::admin_signer().pubkey();
    builder
        .mint(mint)
        .recipient(Pubkey::new_unique())
        .recipient_ata(Pubkey::new_unique())
        .payer(admin)
        .mint_authority(admin)
        .token_program(spl_token::id())
        .amount(1_000)
        .idempotency_memo("contra:mint-idempotency:1".to_string());
    builder
}

/// Test fixture: builds a fresh `SenderState` with a `MockRpcServer`.
/// When `populate_builder` is true, pre-inserts a `MintToBuilder` for
/// `txn_id` so the JIT helper can read it; when false, the helper hits
/// its "no cached builder" early-return branch on first lookup.
struct Fixture {
    state: contra_indexer::operator::sender::types::SenderState,
    mock: MockRpcServer,
    txn_id: i64,
    mint: Pubkey,
    instruction: InstructionWithSigners,
}

async fn build_fixture(populate_builder: bool) -> Fixture {
    ensure_admin_signer_env();
    let mock = MockRpcServer::start().await;
    let mock_storage = MockStorage::new();

    let mint = Pubkey::new_unique();
    // Pre-populate the mint cache so `get_mint_metadata` resolves from
    // storage rather than falling back to RPC. This keeps the per-scenario
    // RPC scripts focused on the JIT helper's own calls (account probe,
    // blockhash, send, confirm, backoff).
    mock_storage.mints.lock().unwrap().insert(
        mint.to_string(),
        DbMint::new(mint.to_string(), 6, spl_token::id().to_string()),
    );

    let storage = Arc::new(Storage::Mock(mock_storage));
    let mut state = test_hooks::new_sender_state(
        &make_config(mock.url(), ProgramType::Escrow),
        CommitmentLevel::Confirmed,
        None,
        storage,
        // retry_max_attempts is unused by JIT itself but feeds RPC retry config.
        1,
        // Tight confirmation poll interval keeps the unconfirmed-then-backoff
        // tests' wall-clock low while still exercising
        // MAX_POLL_ATTEMPTS_CONFIRMATION = 5 retries.
        1,
        None,
    )
    .expect("SenderState construction must succeed under Mock storage");

    let txn_id: i64 = 7;
    if populate_builder {
        state.mint_builders.insert(txn_id, make_mint_builder(mint));
    }

    let instruction = make_passthrough_instruction();
    Fixture {
        state,
        mock,
        txn_id,
        mint,
        instruction,
    }
}

// ─────────────────────────────────────────────────────────────────────
// Mint already initialized — fast-path return.
// ─────────────────────────────────────────────────────────────────────
//
// One getAccountInfo reply with an initialized mint. The helper must
// short-circuit and return Some(instruction) without sending any
// transaction.
#[tokio::test]
async fn jit_returns_some_when_mint_already_initialized() {
    let fixture = build_fixture(true).await;
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    mock.enqueue("getAccountInfo", account_info_reply(&pack_mint_bytes(true)));

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_some(),
        "fast-path on initialized mint must return Some(instruction)"
    );
    assert_eq!(
        mock.call_count("getAccountInfo"),
        1,
        "exactly one initial probe; no JIT send required"
    );
    assert_eq!(mock.call_count("sendTransaction"), 0);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Full happy path — uninit probe → InitializeMint sent → confirmed.
// ─────────────────────────────────────────────────────────────────────
//
// Probe sees uninit bytes, helper builds + sends InitializeMint, the
// confirmation comes back clean, and the helper returns the original
// `instruction` for downstream `mint_to`.
#[tokio::test]
async fn jit_completes_full_initialize_then_returns_some() {
    let fixture = build_fixture(true).await;
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    mock.enqueue("getAccountInfo", account_info_reply(&[0u8; Mint::LEN]));
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    mock.enqueue("getSignatureStatuses", confirmed_status_reply());

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_some(),
        "full happy path must return Some(instruction)"
    );
    assert_eq!(mock.call_count("getAccountInfo"), 1);
    assert_eq!(mock.call_count("getLatestBlockhash"), 1);
    assert_eq!(mock.call_count("sendTransaction"), 1);
    assert_eq!(mock.call_count("getSignatureStatuses"), 1);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Initial probe returns an RPC error — fail-safe falls through to JIT.
// ─────────────────────────────────────────────────────────────────────
//
// A permanent RPC error on the first probe (`-32601` = method-not-found,
// which `RpcClientWithRetry` does NOT retry) flows into the fail-safe
// branch: the helper assumes the mint doesn't exist and continues with
// JIT. We use `-32601` rather than a transient code (e.g. `-32000`)
// because `is_permanent_rpc_error` short-circuits the retry loop, sparing
// the test the ~3 s exponential-backoff wall-clock cost while exercising
// the same source-side `Err(_)` arm.
#[tokio::test]
async fn jit_falls_through_when_initial_probe_returns_rpc_error() {
    let fixture = build_fixture(true).await;
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    mock.enqueue(
        "getAccountInfo",
        Reply::error(-32601, "method not found (simulated)"),
    );
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    mock.enqueue("getSignatureStatuses", confirmed_status_reply());

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_some(),
        "RPC error on probe must not abort JIT — fail-safe branch must continue to send"
    );
    // Probe = 1 (no retry on -32601), then full JIT sequence proceeds.
    assert_eq!(mock.call_count("getAccountInfo"), 1);
    assert_eq!(mock.call_count("sendTransaction"), 1);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// sendTransaction fails — helper returns None without polling for confirmation.
// ─────────────────────────────────────────────────────────────────────
//
// Probe sees uninit, blockhash succeeds, send fails. The helper must
// abort without polling for confirmation. `RetryPolicy::Idempotent`
// (which InitializeMint uses) retries up to 5 times on transient errors;
// we return `-32601` so `is_permanent_rpc_error` short-circuits, the
// helper aborts after a single send attempt, and the test wall-clock
// stays bounded.
#[tokio::test]
async fn jit_returns_none_when_send_transaction_fails() {
    let fixture = build_fixture(true).await;
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    mock.enqueue("getAccountInfo", account_info_reply(&[0u8; Mint::LEN]));
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue(
        "sendTransaction",
        Reply::error(-32601, "method not found (simulated send failure)"),
    );

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_none(),
        "sendTransaction failure must abort JIT and return None"
    );
    assert_eq!(mock.call_count("sendTransaction"), 1);
    assert_eq!(
        mock.call_count("getSignatureStatuses"),
        0,
        "no confirmation poll should run when send itself failed"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Send succeeds but stays unconfirmed — backoff probe finds it initialized.
// ─────────────────────────────────────────────────────────────────────
//
// Probe sees uninit, send succeeds, but the InitializeMint never
// confirms (status returns `null` on every poll). After
// MAX_POLL_ATTEMPTS_CONFIRMATION=5 nulls, `check_transaction_status`
// returns `ConfirmationResult::Retry`, which lands in the
// `mint_is_initialized_on_chain` backoff. The very first backoff
// `getAccountInfo` returns initialized bytes, so the helper treats JIT
// as success and returns `Some(instruction)`.
#[tokio::test]
async fn jit_returns_some_when_backoff_recovers_from_unconfirmed() {
    let fixture = build_fixture(true).await;
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    // Initial probe: uninit (drives JIT).
    mock.enqueue("getAccountInfo", account_info_reply(&[0u8; Mint::LEN]));
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    // 5× pending so check_transaction_status returns Retry.
    for _ in 0..5 {
        mock.enqueue("getSignatureStatuses", null_status_reply());
    }
    // First backoff probe sees the racing InitializeMint settled.
    mock.enqueue("getAccountInfo", account_info_reply(&pack_mint_bytes(true)));

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_some(),
        "race-recovery branch must return Some when backoff observes init"
    );
    assert_eq!(
        mock.call_count("getAccountInfo"),
        2,
        "1 initial probe + 1 backoff probe (succeeded on first attempt)"
    );
    assert_eq!(mock.call_count("sendTransaction"), 1);
    assert_eq!(mock.call_count("getSignatureStatuses"), 5);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Send succeeds but stays unconfirmed — backoff exhausts, helper returns None.
// ─────────────────────────────────────────────────────────────────────
//
// Same scripted prefix as the race-recovery test, but every backoff probe sees
// uninit. After ATTEMPTS=4 backoff polls,
// `mint_is_initialized_on_chain` returns false and the helper logs an
// error and returns None.
//
// Wall-clock note: this is the only test that pays the full
// 4 × BACKOFF_MS = ~750 ms for the backoff loop; do not duplicate this
// shape elsewhere.
#[tokio::test]
async fn jit_returns_none_when_backoff_exhausts_with_uninit() {
    let fixture = build_fixture(true).await;
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    mock.enqueue("getAccountInfo", account_info_reply(&[0u8; Mint::LEN]));
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    for _ in 0..5 {
        mock.enqueue("getSignatureStatuses", null_status_reply());
    }
    // 4 backoff probes (matches ATTEMPTS=4 in mint_is_initialized_on_chain),
    // every one sees uninit so the loop exhausts.
    for _ in 0..4 {
        mock.enqueue("getAccountInfo", account_info_reply(&[0u8; Mint::LEN]));
    }

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_none(),
        "backoff exhaustion with uninit reads must surface as None"
    );
    assert_eq!(
        mock.call_count("getAccountInfo"),
        5,
        "1 initial probe + 4 backoff attempts"
    );
    assert_eq!(mock.call_count("getSignatureStatuses"), 5);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// No cached builder — early return, zero RPC calls.
// ─────────────────────────────────────────────────────────────────────
//
// `state.mint_builders` is empty for the queried txn_id. The helper
// must early-return None without issuing any RPC call.
#[tokio::test]
async fn jit_returns_none_when_no_cached_builder() {
    let fixture = build_fixture(false).await; // do NOT pre-populate.
    let Fixture {
        mut state,
        mock,
        txn_id,
        mint: _mint,
        instruction,
    } = fixture;

    let result = test_hooks::jit_mint_init(&mut state, txn_id, instruction).await;

    assert!(
        result.is_none(),
        "missing builder must short-circuit with None"
    );
    assert_eq!(
        mock.call_count("getAccountInfo"),
        0,
        "early return must skip every RPC call"
    );
    mock.shutdown().await;
}
