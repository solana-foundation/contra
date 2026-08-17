use {
    private_channel_escrow_program_client::PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
    private_channel_withdraw_program_client::PRIVATE_CHANNEL_WITHDRAW_PROGRAM_ID,
    solana_address::Address,
    solana_client::rpc_client::RpcClient,
    solana_rpc::rpc::JsonRpcConfig,
    solana_sdk::signature::Keypair,
    solana_sdk_ids::bpf_loader_upgradeable,
    solana_test_validator::{TestValidator, TestValidatorGenesis, UpgradeableProgramInfo},
    std::{
        fs,
        io::Write,
        net::TcpListener,
        path::{Path, PathBuf},
        sync::Once,
    },
};

fn get_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind to port 0");
    let port = listener
        .local_addr()
        .expect("Failed to get local address")
        .port();
    drop(listener);
    port
}

const ESCROW_PROGRAM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/programs/private_channel_escrow_program.so"
);

const WITHDRAW_PROGRAM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/programs/private_channel_withdraw_program.so"
);

#[cfg(target_os = "macos")]
const GEYSER_PLUGIN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/geyser/libyellowstone_grpc_geyser.dylib"
);

#[cfg(target_os = "linux")]
const GEYSER_PLUGIN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/geyser/libyellowstone_grpc_geyser.so"
);

/// Preflight check: verify the escrow `.so` that solana-test-validator is
/// about to load was built with the same `test-tree` feature flag as the
/// `private-channel-indexer` crate this binary is linked against.
///
/// Why this exists:
///   `target/deploy/private_channel_escrow_program.so` is a single artifact path
///   that both prod (`make build`) and test-tree (`make build-test`) builds
///   overwrite. Meanwhile the size of a generation window is a compile-time
///   constant on the `private-channel-indexer` crate, derived from the same
///   feature. If the two disagree, the operator computes one generation for a
///   nonce and the program enforces another, so every release is rejected and
///   the test times out minutes later on a cryptic balance assertion.
///
///   This preflight reads cargo's build fingerprint for the escrow
///   program, matches it to the deployed `.so` by mtime, and panics with
///   actionable instructions if the feature set doesn't match this crate.
fn verify_escrow_program_features_match_indexer(program_path: &Path) {
    static CHECKED: Once = Once::new();
    CHECKED.call_once(|| {
        // Detect test-tree via the window size: 65,536 in prod, 8 under the feature.
        let expected_test_tree =
            private_channel_indexer::operator::bitmap_constants::NONCES_PER_GENERATION != 65_536;
        let so_mtime = match fs::metadata(program_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "Preflight: cannot stat {}: {e}. Skipping feature check.",
                    program_path.display()
                );
                return;
            }
        };

        // Cargo's sbpf fingerprint lives in target/sbpf-solana-solana/release/
        // .fingerprint/private-channel-escrow-program-<hash>/lib-private_channel_escrow_program.json.
        // The workspace target dir is two parents up from test_utils/programs/.
        let workspace_root = match program_path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            Some(p) => p.to_path_buf(),
            None => {
                tracing::warn!("Preflight: cannot locate workspace root. Skipping.");
                return;
            }
        };
        let fingerprint_dir = workspace_root.join("target/sbpf-solana-solana/release/.fingerprint");
        if !fingerprint_dir.exists() {
            tracing::warn!(
                "Preflight: fingerprint dir {} missing. Skipping feature check.",
                fingerprint_dir.display()
            );
            return;
        }

        // Find the fingerprint JSON whose mtime is closest-before the .so mtime.
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        if let Ok(entries) = fs::read_dir(&fingerprint_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !name_str.starts_with("private-channel-escrow-program-") {
                    continue;
                }
                let json_path = entry.path().join("lib-private_channel_escrow_program.json");
                let Ok(meta) = fs::metadata(&json_path) else {
                    continue;
                };
                let Ok(mtime) = meta.modified() else { continue };
                if mtime > so_mtime {
                    continue; // built after the .so we're loading
                }
                if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                    best = Some((mtime, json_path));
                }
            }
        }

        let Some((_, fp_path)) = best else {
            tracing::warn!("Preflight: no matching fingerprint for deployed escrow .so. Skipping.");
            return;
        };

        let Ok(contents) = fs::read_to_string(&fp_path) else {
            return;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) else {
            return;
        };
        let features = json.get("features").and_then(|v| v.as_str()).unwrap_or("");
        let deployed_test_tree = features.contains("test-tree");

        if deployed_test_tree != expected_test_tree {
            let (deployed_name, expected_name) = (
                if deployed_test_tree {
                    "test-tree"
                } else {
                    "production"
                },
                if expected_test_tree {
                    "test-tree"
                } else {
                    "production"
                },
            );
            let rebuild_cmd = if expected_test_tree {
                "make -C private-channel-escrow-program build-test"
            } else {
                "make -C private-channel-escrow-program build"
            };
            panic!(
                "\n\n\
                ========================================================================\n\
                BITMAP GENERATION FEATURE MISMATCH\n\
                ========================================================================\n\
                Deployed escrow `.so` features: {deployed_name}\n\
                Current test binary expects:    {expected_name}\n\
                \n\
                NONCES_PER_GENERATION is a compile-time constant that depends on the\n\
                `test-tree` feature. It must match the escrow program loaded into\n\
                solana-test-validator, or every release is rejected on-chain with\n\
                NonceOutsideCurrentGeneration.\n\
                \n\
                Rebuild the escrow program to match:\n\
                    {rebuild_cmd}\n\
                ========================================================================\n\n"
            );
        }
    });
}

fn make_program_info(program_id_bytes: [u8; 32], program_path: &str) -> UpgradeableProgramInfo {
    UpgradeableProgramInfo {
        program_id: Address::new_from_array(program_id_bytes),
        loader: bpf_loader_upgradeable::id(),
        upgrade_authority: Address::default(),
        program_path: PathBuf::from(program_path),
    }
}

/// Start the solana-test-validator on free ports with geyser plugin enabled.
/// Returns the test validator instance, the mint keypair, and the geyser port.
pub async fn start_test_validator() -> (TestValidator, Keypair, u16) {
    // Bind to port 0 and let the OS pick a free port for RPC and geyser.
    // Concurrent validators (nextest runs each test in its own process) never
    // collide: the kernel assigns OS-level sockets atomically.
    // All other validator sockets (gossip, TPU, TVU) use port=0 as well.
    let rpc_port = get_free_port();
    let gossip_port = 0u16;
    let geyser_port = get_free_port();

    let rpc_config = JsonRpcConfig {
        rpc_threads: 4,
        rpc_blocking_threads: 4,
        full_api: true,
        disable_health_check: true,
        enable_rpc_transaction_history: true,
        ..Default::default()
    };

    verify_escrow_program_features_match_indexer(Path::new(ESCROW_PROGRAM_PATH));

    let (test_validator, mint_keypair) = tokio::task::spawn_blocking(move || {
        let escrow_program = make_program_info(
            PRIVATE_CHANNEL_ESCROW_PROGRAM_ID.to_bytes(),
            ESCROW_PROGRAM_PATH,
        );
        let withdraw_program = make_program_info(
            PRIVATE_CHANNEL_WITHDRAW_PROGRAM_ID.to_bytes(),
            WITHDRAW_PROGRAM_PATH,
        );

        let geyser_config = serde_json::json!({
            "libpath": GEYSER_PLUGIN_PATH,
            "log": { "level": "info" },
            "grpc": {
                "address": format!("127.0.0.1:{}", geyser_port),
                "channel_capacity": "100_000",
                "unary_concurrency_limit": 100
            }
        });

        let mut temp_config_file =
            tempfile::NamedTempFile::new().expect("Failed to create temp config file");
        temp_config_file
            .write_all(geyser_config.to_string().as_bytes())
            .expect("Failed to write geyser config");

        let mut genesis = TestValidatorGenesis::default();
        genesis.geyser_plugin_config_files = Some(vec![temp_config_file.path().to_path_buf()]);

        genesis
            .rpc_config(rpc_config)
            .rpc_port(rpc_port)
            .gossip_port(gossip_port)
            .add_upgradeable_programs_with_path(&[escrow_program, withdraw_program])
            .start()
    })
    .await
    .expect("Failed to spawn test validator");

    let rpc_url = format!("http://127.0.0.1:{}", rpc_port);
    let client = RpcClient::new(rpc_url);
    if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if client.get_health().is_ok() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    })
    .await
    {
        panic!(
            "Timed out waiting for the test validator to become healthy: {}",
            e
        );
    }

    let mint_keypair = Keypair::try_from(&mint_keypair.to_bytes()[..]).unwrap();
    (test_validator, mint_keypair, geyser_port)
}

/// Start the solana-test-validator without the geyser plugin enabled.
/// Returns the test validator instance and the mint keypair.
pub async fn start_test_validator_no_geyser() -> (TestValidator, Keypair) {
    // Same port strategy as start_test_validator — see the comment there.
    let rpc_port = get_free_port();
    let gossip_port = 0u16;

    let rpc_config = JsonRpcConfig {
        rpc_threads: 4,
        rpc_blocking_threads: 4,
        full_api: true,
        disable_health_check: true,
        enable_rpc_transaction_history: true,
        ..Default::default()
    };

    verify_escrow_program_features_match_indexer(Path::new(ESCROW_PROGRAM_PATH));

    let (test_validator, mint_keypair) = tokio::task::spawn_blocking(move || {
        let escrow_program = make_program_info(
            PRIVATE_CHANNEL_ESCROW_PROGRAM_ID.to_bytes(),
            ESCROW_PROGRAM_PATH,
        );
        let withdraw_program = make_program_info(
            PRIVATE_CHANNEL_WITHDRAW_PROGRAM_ID.to_bytes(),
            WITHDRAW_PROGRAM_PATH,
        );

        let mut genesis = TestValidatorGenesis::default();

        genesis
            .rpc_config(rpc_config)
            .rpc_port(rpc_port)
            .gossip_port(gossip_port)
            .add_upgradeable_programs_with_path(&[escrow_program, withdraw_program])
            .start()
    })
    .await
    .expect("Failed to spawn test validator");

    let client = RpcClient::new(test_validator.rpc_url());
    if let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if client.get_health().is_ok() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    })
    .await
    {
        panic!(
            "Timed out waiting for the test validator to become healthy: {}",
            e
        );
    }

    let mint_keypair = Keypair::try_from(&mint_keypair.to_bytes()[..]).unwrap();
    (test_validator, mint_keypair)
}
