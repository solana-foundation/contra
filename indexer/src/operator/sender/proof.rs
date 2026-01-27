use crate::error::{OperatorError, ProgramError};
use crate::operator::sender::mint;
use crate::operator::tree_constants::MAX_TREE_LEAVES;
use crate::operator::{ReleaseFundsBuilderWithNonce, SIBLING_PROOF_SIZE};
use contra_escrow_program_client::instructions::ResetSmtRootBuilder;
use solana_keychain::Signer;
use solana_sdk::pubkey::Pubkey;
use tracing::{error, info, warn};

use super::types::{InstructionWithSigners, SenderSMTState, SenderState};

impl SenderSMTState {
    pub(super) fn handle_release_funds_transaction(
        &mut self,
        builder_with_nonce: Box<ReleaseFundsBuilderWithNonce>,
        fee_payer: Pubkey,
        signers: Vec<&'static Signer>,
        compute_unit_price: Option<u64>,
        compute_budget: Option<u32>,
    ) -> Result<InstructionWithSigners, OperatorError> {
        let nonce = builder_with_nonce.nonce;
        let transaction_id = builder_with_nonce.transaction_id;
        let mut builder = builder_with_nonce.builder;

        // Check if this nonce expects a different tree than current local tree
        let expected_tree_index = nonce / MAX_TREE_LEAVES as u64;
        let current_tree_index = self.smt_state.tree_index();

        if expected_tree_index != current_tree_index {
            info!(
                "Nonce {} expects tree_index {} but current is {} - will retry after rotation",
                nonce, expected_tree_index, current_tree_index
            );

            return Err(ProgramError::TreeIndexMismatch {
                nonce,
                expected_tree_index,
                current_tree_index,
            }
            .into());
        }

        // Store incomplete builder for potential retry
        self.nonce_to_builder
            .insert(nonce, (transaction_id, builder.clone()));

        // Check if nonce already exists
        if self.smt_state.contains_nonce(nonce) {
            return Err(ProgramError::InvalidProof {
                reason: format!("Nonce {} already exists in SMT", nonce),
            }
            .into());
        }

        // Generate exclusion proof BEFORE inserting nonce
        let exclusion_proof = self.smt_state.generate_exclusion_proof(nonce);

        // Insert nonce into SMT (updates tree state)
        if !self.smt_state.insert_nonce(nonce) {
            return Err(ProgramError::SmtProofFailed {
                reason: format!("Failed to insert nonce {} (already exists)", nonce),
            }
            .into());
        }

        // This will be used for inclusion proof
        let new_root = self.smt_state.current_root();

        let mut sibling_proofs_flat = [0u8; SIBLING_PROOF_SIZE];
        for (i, sibling) in exclusion_proof.iter().enumerate() {
            sibling_proofs_flat[i * 32..(i + 1) * 32].copy_from_slice(sibling);
        }

        builder
            .sibling_proofs(sibling_proofs_flat)
            .new_withdrawal_root(new_root);

        Ok(InstructionWithSigners {
            instructions: vec![builder.instruction()],
            fee_payer,
            signers,
            compute_budget,
            compute_unit_price,
        })
    }
}

/// Check if pending rotation can now be processed
/// Returns the ResetSmtRoot builder if ready to execute
pub fn take_pending_rotation_if_ready(state: &mut SenderState) -> Option<Box<ResetSmtRootBuilder>> {
    state.pending_rotation.as_ref()?;

    // Check if all in-flight transactions are settled
    let has_in_flight = if let Some(ref smt_state) = state.smt_state {
        !smt_state.nonce_to_builder.is_empty()
    } else {
        false
    };

    if !has_in_flight {
        info!("All in-flight transactions settled, rotation ready to execute");
        state.pending_rotation.take()
    } else {
        None
    }
}

/// Rebuild transaction with regenerated SMT proof and retry
pub(super) async fn rebuild_with_regenerated_proof(
    state: &mut SenderState,
    nonce: Option<u64>,
    instruction: InstructionWithSigners,
) -> Option<InstructionWithSigners> {
    error!("InvalidSmtProof detected - rebuilding with new proof");

    let Some(nonce) = nonce else {
        error!("InvalidSmtProof error but not a ReleaseFunds transaction");
        return None;
    };

    let Some(ref mut smt_state) = state.smt_state else {
        error!("No SMT state available");
        return None;
    };

    let Some((transaction_id, builder)) = smt_state.nonce_to_builder.get(&nonce).cloned() else {
        error!("No cached builder found for nonce {}", nonce);
        return None;
    };

    info!(
        "Rebuilding transaction with regenerated proof for nonce {}",
        nonce
    );

    let builder_with_nonce = Box::new(ReleaseFundsBuilderWithNonce {
        builder,
        nonce,
        transaction_id,
    });

    match smt_state.handle_release_funds_transaction(
        builder_with_nonce,
        instruction.fee_payer,
        instruction.signers.clone(),
        instruction.compute_unit_price,
        instruction.compute_budget,
    ) {
        Ok(new_instruction) => {
            info!("Successfully rebuilt transaction with new proof");
            Some(new_instruction)
        }
        Err(e) => {
            error!("Failed to rebuild transaction: {}", e);
            None
        }
    }
}

/// Cleanup SMT state and caches when transaction fails
///
/// Removes the nonce from local SMT to keep it in sync with on-chain state.
/// Also clears builder cache and retry counts.
pub(super) fn cleanup_failed_transaction(state: &mut SenderState, nonce: Option<u64>) {
    if let (Some(nonce), Some(ref mut smt_state)) = (nonce, state.smt_state.as_mut()) {
        if smt_state.smt_state.remove_nonce(nonce) {
            warn!("Rolled back SMT state for failed nonce {}", nonce);
        }
        smt_state.nonce_to_builder.remove(&nonce);
        state.retry_counts.remove(&nonce);
    }

    mint::cleanup_mint_builder(state, nonce.map(|n| n as i64));
}
