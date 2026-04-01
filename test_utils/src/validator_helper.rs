use {
    contra_escrow_program_client::CONTRA_ESCROW_PROGRAM_ID,
    contra_withdraw_program_client::CONTRA_WITHDRAW_PROGRAM_ID,
    solana_address::Address,
    solana_client::rpc_client::RpcClient,
    solana_net_utils::{find_available_port_in_range, sockets::unique_port_range_for_tests},
    solana_rpc::rpc::JsonRpcConfig,
    solana_sdk::signature::Keypair,
    solana_sdk_ids::bpf_loader_upgradeable,
    solana_test_validator::{TestValidator, TestValidatorGenesis, UpgradeableProgramInfo},
    std::{
        io::Write,
        net::{IpAddr, Ipv4Addr, TcpListener},
        path::PathBuf,
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
    "/programs/contra_escrow_program.so"
);

const WITHDRAW_PROGRAM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/programs/contra_withdraw_program.so"
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
    // `unique_port_range_for_tests` uses a process-global AtomicU16 to hand out
    // non-overlapping 200-port blocks.  Under nextest each test is its own process
    // so the atomic always starts at 0 and the NEXTEST_TEST_GLOBAL_SLOT env var
    // (set by nextest) provides per-worker isolation.  Under plain `cargo test`
    // all tests share one process; the non-nextest path of the function simply
    // increments the atomic, giving each concurrent test a distinct block
    // (2000–2200, 2200–2400, …) without any per-slot cap.  Do NOT force-set
    // NEXTEST_TEST_GLOBAL_SLOT here: doing so switches the function into the
    // nextest path which has a 990-port-per-process cap, causing the
    // "Overrunning into the port range" panic when more than ~4 tests run in
    // parallel within the same process.
    let port_range = unique_port_range_for_tests(200);
    let port_range = (port_range.start, port_range.end);
    // Find the first available TCP port within our allocated range for the RPC server.
    let rpc_port = find_available_port_in_range(IpAddr::V4(Ipv4Addr::LOCALHOST), port_range)
        .expect("Failed to find available RPC port in range");
    // gossip_port=0 lets the OS pick a port; port_range below constrains all other validator
    // sockets (TPU, TVU, …) to the same allocated block, keeping them off other tests' ranges.
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

    let (test_validator, mint_keypair) = tokio::task::spawn_blocking(move || {
        let escrow_program =
            make_program_info(CONTRA_ESCROW_PROGRAM_ID.to_bytes(), ESCROW_PROGRAM_PATH);
        let withdraw_program =
            make_program_info(CONTRA_WITHDRAW_PROGRAM_ID.to_bytes(), WITHDRAW_PROGRAM_PATH);

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
            // Constrain all validator sockets to the allocated range so parallel validators
            // stay within their own non-overlapping port blocks.
            .port_range(port_range)
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
    // Same port-isolation strategy as start_test_validator — see the comment there.
    let port_range = unique_port_range_for_tests(200);
    let port_range = (port_range.start, port_range.end);
    let rpc_port = find_available_port_in_range(IpAddr::V4(Ipv4Addr::LOCALHOST), port_range)
        .expect("Failed to find available RPC port in range");
    let gossip_port = 0u16;

    let rpc_config = JsonRpcConfig {
        rpc_threads: 4,
        rpc_blocking_threads: 4,
        full_api: true,
        disable_health_check: true,
        enable_rpc_transaction_history: true,
        ..Default::default()
    };

    let (test_validator, mint_keypair) = tokio::task::spawn_blocking(move || {
        let escrow_program =
            make_program_info(CONTRA_ESCROW_PROGRAM_ID.to_bytes(), ESCROW_PROGRAM_PATH);
        let withdraw_program =
            make_program_info(CONTRA_WITHDRAW_PROGRAM_ID.to_bytes(), WITHDRAW_PROGRAM_PATH);

        let mut genesis = TestValidatorGenesis::default();

        genesis
            .rpc_config(rpc_config)
            .rpc_port(rpc_port)
            .gossip_port(gossip_port)
            // Constrain all validator sockets to the allocated range so parallel validators
            // stay within their own non-overlapping port blocks.
            .port_range(port_range)
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
