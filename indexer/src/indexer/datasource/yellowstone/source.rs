use async_trait::async_trait;
use futures::stream::StreamExt;
use futures::SinkExt;
use solana_sdk::message::VersionedMessage;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
#[cfg(feature = "datasource-rpc")]
use tracing::warn;
use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcClient};
use yellowstone_grpc_proto::convert_from::create_message;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterBlocksMeta, SubscribeRequestFilterTransactions, SubscribeRequestPing,
};

use crate::channel_utils::send_guaranteed;
use crate::config::ProgramType;
use crate::error::{DataSourceError, DataSourceRpcError};
use crate::indexer::datasource::common::parser::escrow::parse_escrow_instruction;
use crate::indexer::datasource::common::parser::withdraw::parse_withdraw_instruction;
use crate::indexer::datasource::common::{datasource::DataSource, types::*};
use crate::indexer::datasource::rpc_polling::types::InnerInstructions;

#[cfg(feature = "datasource-rpc")]
use std::sync::Arc;

#[cfg(feature = "datasource-rpc")]
use crate::indexer::{
    backfill::{fill_slot_range, validate_gap},
    datasource::rpc_polling::rpc::RpcPoller,
};

/// Yellowstone gRPC datasource - directly subscribes to transactions + blocks_meta
pub struct YellowstoneSource {
    endpoint: String,
    x_token: Option<String>,
    commitment: String,
    program_type: ProgramType,
    escrow_instance_id: Option<Pubkey>,
    #[cfg(feature = "datasource-rpc")]
    rpc_poller: Option<Arc<RpcPoller>>,
    #[cfg(feature = "datasource-rpc")]
    max_gap_slots: u64,
    #[cfg(feature = "datasource-rpc")]
    batch_size: usize,
}

impl YellowstoneSource {
    pub fn new(
        endpoint: String,
        x_token: Option<String>,
        commitment: String,
        program_type: ProgramType,
        escrow_instance_id: Option<Pubkey>,
    ) -> Self {
        Self {
            endpoint,
            x_token,
            commitment,
            program_type,
            escrow_instance_id,
            #[cfg(feature = "datasource-rpc")]
            rpc_poller: None,
            #[cfg(feature = "datasource-rpc")]
            max_gap_slots: 0,
            #[cfg(feature = "datasource-rpc")]
            batch_size: 0,
        }
    }

    #[cfg(feature = "datasource-rpc")]
    pub fn with_gap_detection(
        mut self,
        rpc_poller: Arc<RpcPoller>,
        max_gap_slots: u64,
        batch_size: usize,
    ) -> Self {
        self.rpc_poller = Some(rpc_poller);
        self.max_gap_slots = max_gap_slots;
        self.batch_size = batch_size;
        self
    }
}

#[cfg(feature = "datasource-rpc")]
async fn try_fill_reconnect_gap(
    last_seen_slot: u64,
    rpc_poller: &RpcPoller,
    max_gap_slots: u64,
    batch_size: usize,
    program_type: ProgramType,
    escrow_instance_id: Option<Pubkey>,
    instruction_tx: &InstructionSender,
) -> Result<u64, DataSourceError> {
    let current_slot = rpc_poller.get_latest_slot().await.map_err(|e| {
        DataSourceError::GapFillFailed {
            reason: format!("Failed to get latest slot: {}", e),
        }
    })?;

    match validate_gap(current_slot, last_seen_slot, max_gap_slots) {
        Ok(None) => {
            info!(
                "No gap detected on reconnect. Current slot: {}, last seen: {}",
                current_slot, last_seen_slot
            );
            Ok(0)
        }
        Ok(Some(gap)) => {
            info!(
                "Gap detected on reconnect: {} slots (from {} to {}). Backfilling...",
                gap, last_seen_slot, current_slot
            );
            fill_slot_range(
                rpc_poller,
                last_seen_slot,
                current_slot,
                batch_size,
                program_type,
                escrow_instance_id,
                instruction_tx,
            )
            .await
            .map_err(|e| DataSourceError::GapFillFailed {
                reason: e.to_string(),
            })
        }
        Err(e) => Err(DataSourceError::GapFillFailed {
            reason: e.to_string(),
        }),
    }
}

#[async_trait]
impl DataSource for YellowstoneSource {
    async fn start(
        &mut self,
        tx: InstructionSender,
        cancellation_token: CancellationToken,
    ) -> Result<tokio::task::JoinHandle<()>, DataSourceError> {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let program_id = self.program_type.to_pubkey();
        let commitment_level = CommitmentLevel::from_str_name(&self.commitment.to_uppercase())
            .ok_or_else(|| DataSourceError::InvalidCommitment {
                value: self.commitment.clone(),
            })?;

        info!(
            "Starting Yellowstone datasource for program {:?} (ID: {}) at {} (commitment: {:?})",
            self.program_type, program_id, self.endpoint, commitment_level
        );

        let endpoint = self.endpoint.clone();
        let x_token = self.x_token.clone();
        let program_type = self.program_type;
        let escrow_instance_id = self.escrow_instance_id;

        #[cfg(feature = "datasource-rpc")]
        let rpc_poller = self.rpc_poller.clone();
        #[cfg(feature = "datasource-rpc")]
        let max_gap_slots = self.max_gap_slots;
        #[cfg(feature = "datasource-rpc")]
        let batch_size = self.batch_size;

        let handle = tokio::spawn(async move {
            let last_seen_slot = AtomicU64::new(0);

            loop {
                if cancellation_token.is_cancelled() {
                    info!("Yellowstone source received cancellation signal, stopping...");
                    break;
                }

                match connect_and_stream(
                    &endpoint,
                    x_token.clone(),
                    commitment_level,
                    program_type,
                    escrow_instance_id,
                    tx.clone(),
                    cancellation_token.clone(),
                    &last_seen_slot,
                )
                .await
                {
                    Ok(_) => {
                        info!("Yellowstone gRPC stream ended, reconnecting...");
                    }
                    Err(e) => {
                        let error_msg = format!("{}", e);
                        error!(
                            "Yellowstone gRPC error: {}, reconnecting in 5s...",
                            error_msg
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }

                #[cfg(feature = "datasource-rpc")]
                {
                    let seen = last_seen_slot.load(Ordering::Relaxed);
                    if seen > 0 {
                        if let Some(ref poller) = rpc_poller {
                            match try_fill_reconnect_gap(
                                seen,
                                poller,
                                max_gap_slots,
                                batch_size,
                                program_type,
                                escrow_instance_id,
                                &tx,
                            )
                            .await
                            {
                                Ok(filled) => {
                                    if filled > 0 {
                                        info!(
                                            "Reconnect gap-fill complete: {} slots backfilled",
                                            filled
                                        );
                                    }
                                }
                                Err(DataSourceError::GapFillFailed { ref reason })
                                    if reason.contains("Gap too large") =>
                                {
                                    error!(
                                        "Reconnect gap too large (last seen: {}): {}. \
                                         Operator should investigate; next startup backfill will catch it.",
                                        seen, reason
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "Reconnect gap-fill failed (last seen: {}): {}. Continuing reconnect.",
                                        seen, e
                                    );
                                }
                            }
                        }
                    }
                }
            }

            info!("Yellowstone source stopped gracefully");
        });

        Ok(handle)
    }

    async fn shutdown(&mut self) -> Result<(), DataSourceError> {
        info!("Yellowstone source shutdown requested (gRPC connection will be closed by cancellation)");
        Ok(())
    }
}

async fn connect_and_stream(
    endpoint: &str,
    x_token: Option<String>,
    commitment: CommitmentLevel,
    program_type: ProgramType,
    escrow_instance_id: Option<Pubkey>,
    tx: InstructionSender,
    cancellation_token: CancellationToken,
    last_seen_slot: &AtomicU64,
) -> Result<(), DataSourceError> {
    let mut client = GeyserGrpcClient::build_from_shared(endpoint.to_string())
        .map_err(|e| DataSourceRpcError::Protocol {
            reason: e.to_string(),
        })?
        .x_token(x_token)
        .map_err(|e| DataSourceRpcError::Protocol {
            reason: e.to_string(),
        })?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|e| DataSourceRpcError::Protocol {
            reason: e.to_string(),
        })?
        .connect()
        .await
        .map_err(|e| DataSourceRpcError::Protocol {
            reason: e.to_string(),
        })?;

    let program_id = program_type.to_pubkey();

    info!("Connected to Yellowstone gRPC at {}", endpoint);

    // Subscribe to transactions for our program
    // Always put program_id in account_required
    // If escrow_instance_id is provided, also add it to account_required
    let mut account_required = vec![program_id.to_string()];
    if let Some(instance_id) = escrow_instance_id {
        account_required.push(instance_id.to_string());
    }

    let mut transaction_filters = HashMap::new();
    transaction_filters.insert(
        "contra_program".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            signature: None,
            account_include: vec![],
            account_exclude: vec![],
            account_required,
        },
    );

    // Subscribe to ALL block metadata for slot completion
    let mut blocks_meta = HashMap::new();
    blocks_meta.insert(
        "all_blocks_meta".to_string(),
        SubscribeRequestFilterBlocksMeta {},
    );

    let subscribe_request = SubscribeRequest {
        slots: HashMap::new(),
        accounts: HashMap::new(),
        transactions: transaction_filters,
        transactions_status: HashMap::new(),
        entry: HashMap::new(),
        blocks: HashMap::new(),
        blocks_meta,
        commitment: Some(commitment as i32),
        accounts_data_slice: vec![],
        ping: None,
        from_slot: None,
    };

    info!(
        "Subscribing to Yellowstone gRPC with transactions (program: {}) + blocks_meta (all slots)",
        program_id.to_string()
    );

    let (mut subscribe_tx, mut stream) = client
        .subscribe_with_request(Some(subscribe_request))
        .await
        .map_err(|e| DataSourceRpcError::Protocol {
            reason: e.to_string(),
        })?;

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                info!("Yellowstone stream cancelled, closing connection...");
                drop(stream);
                drop(subscribe_tx);
                info!("Yellowstone gRPC connection closed");
                break;
            }
            message = stream.next() => {
                match message {
                    None => break,
                    Some(message) => match message {
            Ok(msg) => match msg.update_oneof {
                Some(UpdateOneof::Transaction(tx_update)) => {
                    if let Err(e) =
                        handle_transaction(tx_update, &program_id, program_type, &tx).await
                    {
                        error!("Error handling transaction: {}", e);
                        // Convert RpcError to DataSourceError for consistency
                        return Err(DataSourceError::Rpc(e));
                    }
                }
                Some(UpdateOneof::BlockMeta(block_meta)) => {
                    last_seen_slot.store(block_meta.slot, Ordering::Relaxed);
                    debug!("Yellowstone BlockMeta for slot {}", block_meta.slot);

                    let res = send_guaranteed(
                        &tx,
                        ProcessorMessage::SlotComplete {
                            slot: block_meta.slot,
                            program_type,
                        },
                        "SlotComplete (yellowstone)",
                    )
                    .await;
                    if let Err(e) = res {
                        error!(
                            "SlotComplete send failed, stopping Yellowstone gracefully: {}",
                            e
                        );
                        break;
                    }
                }
                Some(UpdateOneof::Ping(_)) => {
                    subscribe_tx
                        .send(SubscribeRequest {
                            ping: Some(SubscribeRequestPing { id: 1 }),
                            ..Default::default()
                        })
                        .await
                        .map_err(|e| DataSourceRpcError::Protocol {
                            reason: e.to_string(),
                        })?;
                }
                _ => {}
            },
            Err(error) => {
                error!("Geyser stream error: {error:?}");
                return Err(DataSourceRpcError::Protocol {
                    reason: format!("Stream error: {:?}", error),
                }.into());
            }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_transaction(
    tx_update: yellowstone_grpc_proto::geyser::SubscribeUpdateTransaction,
    program_id: &Pubkey,
    program_type: ProgramType,
    channel: &InstructionSender,
) -> Result<(), DataSourceRpcError> {
    let slot = tx_update.slot;

    let tx_info = tx_update
        .transaction
        .ok_or_else(|| DataSourceRpcError::Protocol {
            reason: "Missing transaction info".to_string(),
        })?;

    let mut inner_instructions_vec: Vec<InnerInstructions> = vec![];

    if let Some(meta) = &tx_info.meta {
        inner_instructions_vec = meta
            .inner_instructions
            .iter()
            .map(|ix_set| InnerInstructions {
                index: ix_set.index as u8,
                instructions: ix_set
                    .instructions
                    .iter()
                    .map(|ix| CompiledInstruction {
                        program_id_index: ix.program_id_index as u8,
                        accounts: ix.accounts.clone(),
                        data: bs58::encode(&ix.data).into_string(),
                    })
                    .collect(),
            })
            .collect();
    }

    // Extract signature
    let signature = bs58::encode(&tx_info.signature).into_string();

    // Convert protobuf transaction to Solana types
    let proto_tx = tx_info
        .transaction
        .ok_or_else(|| DataSourceRpcError::Protocol {
            reason: "Missing transaction".to_string(),
        })?;
    let proto_message = proto_tx
        .message
        .ok_or_else(|| DataSourceRpcError::Protocol {
            reason: "Missing message".to_string(),
        })?;
    let versioned_message =
        create_message(proto_message).map_err(|e| DataSourceRpcError::Protocol {
            reason: format!("Failed to create message: {}", e),
        })?;

    // Get account keys and instructions
    let (account_keys, instructions): (
        Vec<Pubkey>,
        Vec<solana_sdk::message::compiled_instruction::CompiledInstruction>,
    ) = match &versioned_message {
        VersionedMessage::Legacy(msg) => (msg.account_keys.clone(), msg.instructions.clone()),
        VersionedMessage::V0(msg) => (msg.account_keys.clone(), msg.instructions.clone()),
    };

    info!(
        "Yellowstone received transaction at slot {}, signature: {}, {} instructions",
        slot,
        signature,
        instructions.len()
    );

    // Parse each instruction that belongs to our program
    for instruction in instructions {
        let program_id_index = instruction.program_id_index as usize;
        if program_id_index >= account_keys.len() {
            error!(
                "Invalid program_id_index {} for transaction {}",
                program_id_index, signature
            );
            continue;
        }

        let ix_program_id = account_keys[program_id_index];
        if ix_program_id != *program_id {
            continue; // Not our program
        }

        // Convert to our CompiledInstruction type (from types.rs)
        let compiled_ix = CompiledInstruction {
            program_id_index: instruction.program_id_index,
            accounts: instruction.accounts.clone(),
            data: bs58::encode(&instruction.data).into_string(),
        };

        // Parse instruction based on program type and handle immediately to avoid Send issues
        let instruction_data = match program_type {
            ProgramType::Escrow => {
                match parse_escrow_instruction(&compiled_ix, &account_keys, &inner_instructions_vec)
                {
                    Ok(Some(inst)) => Some(ProgramInstruction::Escrow(Box::new(inst))),
                    Ok(None) => {
                        debug!(
                            "Yellowstone: Unsupported escrow instruction at slot {}",
                            slot
                        );
                        None
                    }
                    Err(e) => {
                        error!("Failed to parse escrow instruction at slot {}: {}", slot, e);
                        None
                    }
                }
            }
            ProgramType::Withdraw => {
                match parse_withdraw_instruction(
                    &compiled_ix,
                    &account_keys,
                    &inner_instructions_vec,
                ) {
                    Ok(Some(inst)) => Some(ProgramInstruction::Withdraw(Box::new(inst))),
                    Ok(None) => {
                        debug!(
                            "Yellowstone: Unsupported withdraw instruction at slot {}",
                            slot
                        );
                        None
                    }
                    Err(e) => {
                        error!(
                            "Failed to parse withdraw instruction at slot {}: {}",
                            slot, e
                        );
                        None
                    }
                }
            }
        };

        if let Some(instruction_data) = instruction_data {
            let instruction_meta = InstructionWithMetadata {
                instruction: instruction_data,
                slot,
                program_type,
                signature: Some(signature.clone()),
            };

            let res = send_guaranteed(
                channel,
                ProcessorMessage::Instruction(instruction_meta),
                "instruction (yellowstone)",
            )
            .await;
            if let Err(e) = res {
                return Err(DataSourceRpcError::Protocol {
                    reason: format!("Instruction send failed: {}", e),
                });
            }
        }
    }

    Ok(())
}
