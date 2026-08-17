use crate::error::OperatorError;
use crate::operator::bitmap_constants::NONCES_PER_GENERATION;
use crate::operator::sender::mint;
use crate::operator::{ReleaseFundsBuilderWithNonce, TransactionKind};
use private_channel_escrow_program_client::instructions::RotateBitmapBuilder;
use solana_keychain::Signer;
use solana_sdk::pubkey::Pubkey;
use tracing::{error, info, warn};

use super::transaction::{classify_generation, park_release_for_rotation, GenerationWindow};
use super::types::{InstructionWithSigners, SenderState, TransactionContext};

impl SenderState {
    /// Build a signable release, unless the bitmap's window says it is doomed.
    ///
    /// The program refuses a nonce from another generation, so a release built
    /// here can be worth nothing before it is ever signed. This check is only an
    /// optimisation in front of that refusal, which stays the authority and
    /// still handles anything this check waves through.
    pub(super) async fn handle_release_funds_transaction(
        &mut self,
        builder_with_nonce: Box<ReleaseFundsBuilderWithNonce>,
        fee_payer: Pubkey,
        signers: Vec<&'static Signer>,
        compute_unit_price: Option<u64>,
        compute_budget: Option<u32>,
    ) -> Result<InstructionWithSigners, OperatorError> {
        let nonce = builder_with_nonce.nonce;
        let transaction_id = builder_with_nonce.transaction_id;
        let trace_id = builder_with_nonce.trace_id;
        let builder = builder_with_nonce.builder;

        let (window, chain_generation) = self.release_window(nonce).await;
        let instruction = InstructionWithSigners {
            instructions: vec![builder.instruction()],
            fee_payer,
            signers,
            compute_budget,
            compute_unit_price,
        };

        let nonce_generation = nonce / NONCES_PER_GENERATION;
        match window {
            GenerationWindow::Open => {
                self.in_flight_withdrawals.insert(nonce);
                Ok(instruction)
            }
            // Nothing in a release depends on the generation, so the instruction
            // built here is exactly the one the rotation makes valid. The nonce
            // stays out of the in-flight set on purpose: that set is what the
            // rotation waits on, and waiting on this nonce would hold back the
            // very rotation that unblocks it.
            GenerationWindow::NotYetOpen => {
                // The wait has to be durable before it is queued: an unparked
                // entry would leave the in-memory queue as the only record of a
                // release that was never broadcast.
                if !park_release_for_rotation(&self.storage, transaction_id, nonce).await {
                    return Err(generation_mismatch(
                        nonce,
                        nonce_generation,
                        chain_generation,
                    ));
                }
                info!(
                    nonce,
                    nonce_generation,
                    chain_generation,
                    "Holding the release until its window opens"
                );
                self.rotation_retry_queue.push((
                    TransactionContext {
                        kind: TransactionKind::ReleaseFunds,
                        transaction_id: Some(transaction_id),
                        withdrawal_nonce: Some(nonce),
                        trace_id: Some(trace_id),
                    },
                    instruction,
                ));
                Err(generation_mismatch(
                    nonce,
                    nonce_generation,
                    chain_generation,
                ))
            }
            GenerationWindow::Closed => {
                error!(
                    nonce,
                    nonce_generation,
                    chain_generation,
                    "Not sending a release whose generation the bitmap has rotated past"
                );
                Err(generation_mismatch(
                    nonce,
                    nonce_generation,
                    chain_generation,
                ))
            }
        }
    }

    /// Where a nonce sits relative to the bitmap's current generation.
    ///
    ///   cache holds this generation  -> Open                    no RPC
    ///   anything else                -> read the chain          Open | NotYetOpen | Closed
    ///   the read failed              -> Open                    let the program judge
    ///
    /// Only a fresh read is allowed to say no. The cache can lag the chain, and a
    /// lagging cache that refused a withdrawal would strand it forever: unsent
    /// means unrejected, and that rejection is what would have corrected the cache.
    pub(super) async fn release_window(&mut self, nonce: u64) -> (GenerationWindow, u64) {
        let nonce_generation = nonce / NONCES_PER_GENERATION;

        if self.cached_generation == Some(nonce_generation) {
            return (GenerationWindow::Open, nonce_generation);
        }

        match self.refresh_generation().await {
            Ok(chain_generation) => (
                classify_generation(nonce, chain_generation),
                chain_generation,
            ),
            // An unread bitmap refuses nothing. Without a fresh answer the only
            // choice that cannot strand a releasable withdrawal is to broadcast
            // it and let the program judge.
            Err(e) => {
                warn!(nonce, "Sending without a generation check: {e}");
                (GenerationWindow::Open, nonce_generation)
            }
        }
    }
}

fn generation_mismatch(nonce: u64, nonce_generation: u64, chain_generation: u64) -> OperatorError {
    crate::error::ProgramError::GenerationMismatch {
        nonce,
        nonce_generation,
        chain_generation,
    }
    .into()
}

/// Check if pending rotation can now be processed.
/// Returns the RotateBitmap builder if ready to execute.
///
/// Two things hold a rotation back, and both are about the bits it would erase.
/// An in-flight release still has a nonce the chain has not recorded yet, and a
/// deferred remint has a nonce whose bit is the only proof of whether the user
/// was already paid. Wiping either turns a decision the chain can answer into
/// one it cannot.
pub async fn take_pending_rotation_if_ready(
    state: &mut SenderState,
) -> Option<Box<RotateBitmapBuilder>> {
    state.pending_rotation.as_ref()?;

    if !state.in_flight_withdrawals.is_empty() {
        return None;
    }

    if !pending_remints_have_settled(state).await {
        return None;
    }

    info!("All in-flight transactions settled, rotation ready to execute");
    state.pending_rotation.take()
}

/// True when no deferred remint still depends on a bit this rotation would clear.
///
/// The generation is read rather than remembered, and only when a remint is
/// actually outstanding, so the ordinary rotation pays for no extra round trip.
/// A failed read leaves us unable to say which nonces the rotation would erase,
/// so it holds the rotation: the next tick tries again, whereas a bit cleared
/// out from under a maturing remint never comes back.
async fn pending_remints_have_settled(state: &SenderState) -> bool {
    let pending_nonces: Vec<u64> = state
        .pending_remints
        .iter()
        .filter_map(|entry| entry.ctx.withdrawal_nonce)
        .collect();

    if pending_nonces.is_empty() {
        return true;
    }

    let generation = match state.fetch_current_generation().await {
        Ok(generation) => generation,
        Err(e) => {
            warn!("Holding the rotation: could not read the current generation: {e}");
            return false;
        }
    };

    let blocking = pending_nonces
        .iter()
        .find(|nonce| **nonce / NONCES_PER_GENERATION == generation);

    if let Some(nonce) = blocking {
        info!(
            generation,
            "Holding the rotation: nonce {nonce} still has a remint waiting on its bit"
        );
        return false;
    }

    true
}

/// Drop the caches a failed withdrawal owns.
///
/// The nonce leaves the in-flight set so a queued rotation is no longer blocked
/// by it. Nothing else needs undoing: the chain records consumption, so there is
/// no local replay state that could drift from it.
pub(super) fn cleanup_failed_transaction(state: &mut SenderState, nonce: Option<u64>) {
    if let Some(nonce) = nonce {
        state.in_flight_withdrawals.remove(&nonce);
        state.retry_counts.remove(&nonce);
        // Note: when called from handle_permanent_failure, remint_cache is
        // already drained. This removal is defensive for any other call site.
        state.remint_cache.remove(&nonce);
    }

    mint::cleanup_mint_builder(state, nonce.map(|n| n as i64));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProgramError;
    use crate::operator::sender::test_support::{
        mock_bitmap_account, mock_bitmap_read_failure, mock_bitmap_sequence,
        mock_with_processing_row, push_processing_row, row_status, sender_state,
        sender_state_with_storage,
    };
    use crate::operator::sender::transaction::handle_nonce_outside_generation;
    use crate::operator::sender::types::{PendingRemint, PendingSig, TransactionContext};
    use crate::operator::utils::instruction_util::WithdrawalRemintInfo;
    use crate::storage::common::models::TransactionStatus;
    use crate::storage::common::storage::mock::MockStorage;
    use private_channel_escrow_program_client::instructions::ReleaseFundsBuilder;
    use solana_sdk::signature::Signature;
    use std::sync::atomic::Ordering;
    use tokio::sync::mpsc;

    fn make_test_remint_info(transaction_id: i64, trace_id: &str) -> WithdrawalRemintInfo {
        WithdrawalRemintInfo {
            transaction_id,
            trace_id: trace_id.to_string(),
            mint: Pubkey::new_unique(),
            user: Pubkey::new_unique(),
            user_ata: Pubkey::new_unique(),
            token_program: spl_token::id(),
            amount: 1000,
        }
    }

    fn make_release_funds_builder() -> ReleaseFundsBuilder {
        let mut b = ReleaseFundsBuilder::new();
        let pk = Pubkey::new_unique();
        b.payer(pk)
            .operator(pk)
            .instance(pk)
            .withdrawal_bitmap(pk)
            .operator_pda(pk)
            .mint(pk)
            .allowed_mint(pk)
            .user_ata(pk)
            .instance_ata(pk)
            .token_program(spl_token::id())
            .user(pk)
            .amount(1000)
            .transaction_nonce(0);
        b
    }

    fn rotation_builder() -> Box<RotateBitmapBuilder> {
        let mut b = RotateBitmapBuilder::new();
        let pk = Pubkey::new_unique();
        b.payer(pk)
            .operator(pk)
            .instance(pk)
            .withdrawal_bitmap(pk)
            .operator_pda(pk)
            .expected_generation(0);
        Box::new(b)
    }

    /// Queue a matured deferred remint for `nonce`.
    fn queue_pending_remint(state: &mut SenderState, nonce: u64) {
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                kind: TransactionKind::ReleaseFunds,
                transaction_id: Some(1),
                withdrawal_nonce: Some(nonce),
                trace_id: Some("t".to_string()),
            },
            remint_info: make_test_remint_info(1, "t"),
            signatures: Vec::new(),
            original_error: "release_funds failed".to_string(),
            deadline: chrono::Utc::now(),
            finality_check_attempts: 0,
            release_refused_on_chain: false,
            coverage_slot: None,
        });
    }

    // ── take_pending_rotation_if_ready ───────────────────────────────

    #[tokio::test]
    async fn rotation_returns_none_when_no_pending() {
        let mut state = sender_state("http://localhost:8899");
        assert!(take_pending_rotation_if_ready(&mut state).await.is_none());
    }

    #[tokio::test]
    async fn rotation_returns_builder_when_no_inflight() {
        let mut state = sender_state("http://localhost:8899");
        state.pending_rotation = Some(rotation_builder());

        assert!(take_pending_rotation_if_ready(&mut state).await.is_some());
        assert!(state.pending_rotation.is_none(), "should be taken");
    }

    /// Rotation clears every bit, so it must not overtake a release whose nonce
    /// is still in flight; that nonce would become replayable.
    #[tokio::test]
    async fn rotation_blocked_by_inflight_withdrawal() {
        let mut state = sender_state("http://localhost:8899");
        state.pending_rotation = Some(rotation_builder());
        state.in_flight_withdrawals.insert(0);

        assert!(take_pending_rotation_if_ready(&mut state).await.is_none());
        assert!(state.pending_rotation.is_some(), "should NOT be taken yet");
    }

    #[tokio::test]
    async fn rotation_ready_after_inflight_cleared() {
        let mut state = sender_state("http://localhost:8899");
        state.pending_rotation = Some(rotation_builder());
        state.in_flight_withdrawals.insert(0);
        state.in_flight_withdrawals.remove(&0);

        assert!(take_pending_rotation_if_ready(&mut state).await.is_some());
    }

    /// A deferred remint has left the in-flight set but its outcome still turns
    /// on the bit for its nonce. Rotating now wipes that bit, so the gate that
    /// decides whether to credit the user would be reading a blank window and
    /// would have to give up on the only answer it trusts.
    #[tokio::test]
    async fn rotation_blocked_by_pending_remint_in_the_current_generation() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 0, &[]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.pending_rotation = Some(rotation_builder());
        queue_pending_remint(&mut state, 3);

        assert!(take_pending_rotation_if_ready(&mut state).await.is_none());
        assert!(state.pending_rotation.is_some(), "should NOT be taken yet");
    }

    /// Only the generation about to be wiped matters. An older nonce's bit is
    /// already gone, so holding the rotation for it would close the next
    /// generation to every withdrawal in it and repair nothing at all for the
    /// nonce it was held on behalf of.
    #[tokio::test]
    async fn rotation_ready_when_pending_remint_belongs_to_an_older_generation() {
        use crate::operator::bitmap_constants::NONCES_PER_GENERATION;

        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 1, &[]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.pending_rotation = Some(rotation_builder());
        queue_pending_remint(&mut state, NONCES_PER_GENERATION - 1);

        assert!(take_pending_rotation_if_ready(&mut state).await.is_some());
    }

    /// Not knowing which generation the chain is on means not knowing whether
    /// the rotation would erase evidence a pending remint still needs, and a
    /// cleared bit never comes back, so the rotation waits for a reading.
    #[tokio::test]
    async fn rotation_blocked_when_the_generation_cannot_be_read() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_read_failure(&mut server);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.pending_rotation = Some(rotation_builder());
        queue_pending_remint(&mut state, 3);

        assert!(take_pending_rotation_if_ready(&mut state).await.is_none());
        assert!(state.pending_rotation.is_some(), "should NOT be taken yet");
    }

    // ── cleanup_failed_transaction ───────────────────────────────────

    /// A failed withdrawal must release the rotation barrier it was holding,
    /// otherwise a queued rotation never fires.
    #[test]
    fn cleanup_clears_inflight_and_caches() {
        let mut state = sender_state("http://localhost:8899");
        state.in_flight_withdrawals.insert(5);
        state.retry_counts.insert(5, 2);
        state.remint_cache.insert(5, make_test_remint_info(1, "t"));

        cleanup_failed_transaction(&mut state, Some(5));

        assert!(!state.in_flight_withdrawals.contains(&5));
        assert!(!state.retry_counts.contains_key(&5));
        assert!(!state.remint_cache.contains_key(&5));
    }

    #[test]
    fn cleanup_with_none_nonce_is_noop() {
        let mut state = sender_state("http://localhost:8899");
        state.in_flight_withdrawals.insert(5);

        cleanup_failed_transaction(&mut state, None);

        assert!(state.in_flight_withdrawals.contains(&5));
    }

    // ── handle_release_funds_transaction ─────────────────────────────

    /// A release for `nonce`, carrying the remint info every withdrawal has.
    /// The row id every built release in these tests belongs to.
    const RELEASE_TXID: i64 = 1;

    fn release_with_nonce(nonce: u64) -> Box<ReleaseFundsBuilderWithNonce> {
        Box::new(ReleaseFundsBuilderWithNonce {
            builder: make_release_funds_builder(),
            nonce,
            transaction_id: RELEASE_TXID,
            trace_id: "t".to_string(),
            remint_info: Some(make_test_remint_info(1, "t")),
        })
    }

    /// Run the release build path for `nonce`.
    async fn build_release(
        state: &mut SenderState,
        nonce: u64,
    ) -> Result<InstructionWithSigners, OperatorError> {
        state
            .handle_release_funds_transaction(
                release_with_nonce(nonce),
                Pubkey::new_unique(),
                vec![],
                None,
                None,
            )
            .await
    }

    /// The deadlock guard, and the reason the cache is trusted asymmetrically.
    ///
    /// A cache that has not seen the rotation yet says this nonce is out of the
    /// window. Refusing on that answer alone would never send the withdrawal,
    /// so it would never be rejected, so nothing would ever correct the cache
    /// that refused it: the withdrawal is stuck for good.
    #[tokio::test]
    async fn stale_cache_does_not_withhold_a_nonce_the_chain_has_rotated_into() {
        let mut server = mockito::Server::new_async().await;
        let (_bitmap, reads) = mock_bitmap_sequence(&mut server, vec![(1, Vec::new())]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.cached_generation = Some(0);
        let nonce = NONCES_PER_GENERATION;

        let result = build_release(&mut state, nonce).await;

        assert!(
            result.is_ok(),
            "the chain has rotated into this nonce's window, so it must be sent"
        );
        assert!(state.in_flight_withdrawals.contains(&nonce));
        assert!(state.rotation_retry_queue.is_empty());
        assert_eq!(reads.load(Ordering::SeqCst), 1, "one confirming read");
        assert_eq!(state.cached_generation, Some(1), "the read is remembered");
    }

    /// A cache that wrongly permits a send is no worse than having no cache at
    /// all. The release is broadcast, the program refuses it, and the rejection
    /// path routes the withdrawal to its compensating remint and corrects the
    /// cache on the way through.
    #[tokio::test]
    async fn stale_cache_that_permits_a_send_degrades_to_the_rejection_path() {
        let mut server = mockito::Server::new_async().await;
        let (_bitmap, reads) = mock_bitmap_sequence(&mut server, vec![(1, Vec::new())]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        // The chain rotated to generation 1 without the cache hearing about it.
        state.cached_generation = Some(0);
        state.remint_cache.insert(0, make_test_remint_info(1, "t"));
        state.pending_signatures.insert(
            0,
            vec![PendingSig {
                signature: Signature::new_unique(),
                last_valid_block_height: 1,
            }],
        );

        let instruction = build_release(&mut state, 0)
            .await
            .expect("a matching cache permits the send");
        assert_eq!(reads.load(Ordering::SeqCst), 0, "the cache answered");

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            kind: TransactionKind::ReleaseFunds,
            transaction_id: Some(1),
            withdrawal_nonce: Some(0),
            trace_id: Some("t".to_string()),
        };
        handle_nonce_outside_generation(
            &mut state,
            &ctx,
            Signature::new_unique(),
            instruction,
            &tx,
        )
        .await;

        assert_eq!(
            state.pending_remints.len(),
            1,
            "the rejection path still compensates the withdrawal"
        );
        assert!(rx.try_recv().is_err(), "no terminal status was written");
        assert_eq!(
            state.cached_generation,
            Some(1),
            "the rejection path refreshed the stale cache"
        );
        assert!(!state.in_flight_withdrawals.contains(&0));
    }

    /// An unknown cache is not a refusal. It has to be resolved against the
    /// chain first, or the first withdrawal after a restart would be written off
    /// for no reason other than the operator having just started and not yet
    /// having looked.
    #[tokio::test]
    async fn unknown_cache_verifies_against_the_chain_before_refusing() {
        let mut server = mockito::Server::new_async().await;
        let (_bitmap, reads) = mock_bitmap_sequence(&mut server, vec![(2, Vec::new())]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.cached_generation = None;
        let nonce = 2 * NONCES_PER_GENERATION;

        let result = build_release(&mut state, nonce).await;

        assert!(result.is_ok(), "the nonce is inside the current window");
        assert!(state.in_flight_withdrawals.contains(&nonce));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(state.cached_generation, Some(2));
    }

    /// The steady state: an agreeing cache costs no round trip.
    #[tokio::test]
    async fn warm_matching_cache_sends_without_reading_the_bitmap() {
        let mut server = mockito::Server::new_async().await;
        let (_bitmap, reads) = mock_bitmap_sequence(&mut server, vec![(0, Vec::new())]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.cached_generation = Some(0);

        let result = build_release(&mut state, 7).await;

        assert!(result.is_ok());
        assert!(state.in_flight_withdrawals.contains(&7));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "a warm matching cache must cost no RPC"
        );
    }

    /// A nonce whose window has not opened is held for the rotation that opens
    /// it, and the row records that wait: the queue dies with the process, so
    /// the database has to be the copy a restart can still find.
    #[tokio::test]
    async fn nonce_ahead_of_the_window_is_queued_for_the_rotation() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 0, &[]);

        let mock = mock_with_processing_row(RELEASE_TXID);
        let mut state = sender_state_with_storage(&server.url(), mock.clone());
        state.instance_pda = Some(Pubkey::new_unique());
        let nonce = NONCES_PER_GENERATION;

        let result = build_release(&mut state, nonce).await;

        assert!(matches!(
            result,
            Err(OperatorError::Program(ProgramError::GenerationMismatch {
                nonce_generation: 1,
                chain_generation: 0,
                ..
            }))
        ));
        assert_eq!(state.rotation_retry_queue.len(), 1);
        assert_eq!(
            state.rotation_retry_queue[0].0.withdrawal_nonce,
            Some(nonce)
        );
        assert_eq!(
            row_status(&mock, RELEASE_TXID),
            Some(TransactionStatus::Parked),
            "a queued release must be durable, not only in memory"
        );
        assert!(
            !state.in_flight_withdrawals.contains(&nonce),
            "a queued withdrawal must not hold the rotation barrier"
        );
    }

    /// The queue is only safe because a parked row backs every entry. A park the
    /// database refused leaves nothing behind it, so queueing anyway would
    /// recreate the in-memory-only state while looking like it was fixed.
    #[tokio::test]
    async fn a_release_whose_park_was_refused_is_not_queued() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 0, &[]);

        // No row at all, so the park CAS finds nothing to move.
        let mut state = sender_state_with_storage(&server.url(), MockStorage::new());
        state.instance_pda = Some(Pubkey::new_unique());

        let _ = build_release(&mut state, NONCES_PER_GENERATION).await;

        assert!(
            state.rotation_retry_queue.is_empty(),
            "an unparked release must be left to recovery, not held in memory"
        );
    }

    /// An unreadable park is not a park. Absence of an error says nothing about
    /// whether the row moved, so only a positive answer may queue.
    #[tokio::test]
    async fn a_release_whose_park_errored_is_not_queued() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 0, &[]);

        let mock = mock_with_processing_row(RELEASE_TXID);
        mock.set_should_fail("try_park_processing", true);
        let mut state = sender_state_with_storage(&server.url(), mock);
        state.instance_pda = Some(Pubkey::new_unique());

        let _ = build_release(&mut state, NONCES_PER_GENERATION).await;

        assert!(
            state.rotation_retry_queue.is_empty(),
            "an unconfirmed park must not queue financial work"
        );
    }

    /// The queue and the parked rows are one fact recorded twice, so the
    /// invariant has to hold across every producer rather than per call site.
    /// Both feed the same queue, and a rotation that never lands leaves whatever
    /// they queued sitting there until a restart is the only thing left to find
    /// it.
    #[tokio::test]
    async fn every_queued_release_has_a_parked_row_behind_it() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 0, &[]);

        // One row for the pre-send hold, one for the on-chain refusal, and one
        // whose park is refused because it is not there at all.
        const REFUSED_ROW: i64 = 2;
        let mock = mock_with_processing_row(RELEASE_TXID);
        push_processing_row(&mock, REFUSED_ROW);
        let mut state = sender_state_with_storage(&server.url(), mock.clone());
        state.instance_pda = Some(Pubkey::new_unique());

        let ahead = NONCES_PER_GENERATION;
        let _ = build_release(&mut state, ahead).await;

        let instruction = InstructionWithSigners {
            instructions: vec![],
            fee_payer: Pubkey::new_unique(),
            signers: vec![],
            compute_budget: None,
            compute_unit_price: None,
        };
        let (tx, _rx) = mpsc::channel(10);
        for (transaction_id, nonce) in [(REFUSED_ROW, ahead + 1), (404, ahead + 2)] {
            state.cached_generation = None;
            let ctx = TransactionContext {
                kind: TransactionKind::ReleaseFunds,
                transaction_id: Some(transaction_id),
                withdrawal_nonce: Some(nonce),
                trace_id: Some("t".to_string()),
            };
            handle_nonce_outside_generation(
                &mut state,
                &ctx,
                Signature::new_unique(),
                instruction.clone(),
                &tx,
            )
            .await;
        }

        assert_eq!(
            state.rotation_retry_queue.len(),
            2,
            "the row that could not be parked must not be queued"
        );
        for (ctx, _) in &state.rotation_retry_queue {
            let transaction_id = ctx.transaction_id.expect("a queued release names its row");
            assert_eq!(
                row_status(&mock, transaction_id),
                Some(TransactionStatus::Parked),
                "queued release {transaction_id} has no parked row behind it"
            );
        }
    }

    /// A nonce the chain rotated past is not queued: no rotation brings it back.
    #[tokio::test]
    async fn nonce_behind_the_window_is_refused_and_not_queued() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 1, &[]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());

        let result = build_release(&mut state, 0).await;

        assert!(matches!(
            result,
            Err(OperatorError::Program(ProgramError::GenerationMismatch {
                nonce_generation: 0,
                chain_generation: 1,
                ..
            }))
        ));
        assert!(state.rotation_retry_queue.is_empty());
        assert!(!state.in_flight_withdrawals.contains(&0));
    }

    /// The cache tracks the chain, never the traffic it happens to be handed.
    #[tokio::test]
    async fn cache_never_advances_past_what_the_chain_reported() {
        let mut server = mockito::Server::new_async().await;
        let _bitmap = mock_bitmap_account(&mut server, 0, &[]);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        state.cached_generation = Some(0);

        let result = build_release(&mut state, 5 * NONCES_PER_GENERATION).await;

        assert!(result.is_err());
        assert_eq!(
            state.cached_generation,
            Some(0),
            "only the chain moves the cache"
        );
    }

    /// An unreadable bitmap cannot refuse anything; the program still judges.
    #[tokio::test]
    async fn unreadable_bitmap_sends_rather_than_withholds() {
        let mut server = mockito::Server::new_async().await;
        let _down = mock_bitmap_read_failure(&mut server);

        let mut state = sender_state(&server.url());
        state.instance_pda = Some(Pubkey::new_unique());
        let nonce = NONCES_PER_GENERATION;

        let result = build_release(&mut state, nonce).await;

        assert!(result.is_ok(), "a failed read must not withhold a release");
        assert!(state.in_flight_withdrawals.contains(&nonce));
        assert_eq!(state.cached_generation, None, "nothing was learned");
    }

    #[tokio::test]
    async fn handle_release_funds_marks_nonce_in_flight() {
        let mut state = sender_state("http://localhost:8899");
        state.cached_generation = Some(0);
        let bwn = Box::new(ReleaseFundsBuilderWithNonce {
            builder: make_release_funds_builder(),
            nonce: 0,
            transaction_id: 42,
            trace_id: "trace-42".to_string(),
            remint_info: Some(make_test_remint_info(42, "trace-42")),
        });

        let ix = state
            .handle_release_funds_transaction(
                bwn,
                Pubkey::new_unique(),
                vec![],
                Some(5000),
                Some(200_000),
            )
            .await
            .expect("builder is complete");

        assert!(state.in_flight_withdrawals.contains(&0));
        assert_eq!(ix.instructions.len(), 1);
        assert_eq!(ix.compute_unit_price, Some(5000));
        assert_eq!(ix.compute_budget, Some(200_000));
    }
}
