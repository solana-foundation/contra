use const_crypto::sha2::Sha256;
use std::collections::{HashMap, HashSet};

use crate::operator::{
    tree_constants::{EMPTY_SUBTREE_HASHES, EMPTY_TREE_ROOT, MAX_TREE_LEAVES, TREE_HEIGHT},
    NON_EMPTY_LEAF_HASH,
};

/// In-memory SMT state tracker for a specific instance
/// Tracks which nonces have been inserted into the current tree
/// and maintains incremental tree state for efficient proof generation
#[derive(Debug, Clone)]
pub struct SmtState {
    /// Current tree index (increments on reset)
    tree_index: u64,
    /// Set of nonces that have been inserted into the current tree
    /// Nonces are transaction IDs from the database
    nonces: HashSet<u64>,
    /// Current SMT root hash
    current_root: [u8; 32],
    /// Cache sibling hashes at each level for efficient proof generation
    /// Only stores occupied positions (sparse tree optimization)
    level_caches: [HashMap<usize, [u8; 32]>; TREE_HEIGHT],
}

impl SmtState {
    /// Create a new SMT state for a specific tree index
    pub fn new(tree_index: u64) -> Self {
        Self {
            tree_index,
            nonces: HashSet::new(),
            current_root: EMPTY_TREE_ROOT,
            level_caches: Default::default(),
        }
    }

    /// Hash two 32-byte values together (must match on-chain implementation)
    fn hash_combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(left);
        combined[32..].copy_from_slice(right);
        Self::safe_sha256(&combined)
    }

    /// Compute SHA256 hash
    fn safe_sha256(input: &[u8]) -> [u8; 32] {
        Sha256::new().update(input).finalize()
    }

    /// Get the current tree index
    pub fn tree_index(&self) -> u64 {
        self.tree_index
    }

    /// Insert a nonce into the SMT and update the tree incrementally
    /// Returns true if the nonce was newly inserted, false if it already existed
    ///
    /// This method:
    /// 1. Checks if nonce already exists (idempotent)
    /// 2. Walks tree from leaf to root, caching sibling hashes
    /// 3. Updates current_root
    ///
    /// Complexity: O(TREE_HEIGHT) = O(16) per insert
    pub fn insert_nonce(&mut self, nonce: u64) -> bool {
        if !self.nonces.insert(nonce) {
            return false;
        }

        // Calculate leaf position in tree
        let leaf_position = nonce as usize % MAX_TREE_LEAVES;

        // Start with non-empty leaf hash (what we store for present nonces)
        let mut current_hash = NON_EMPTY_LEAF_HASH;
        let mut current_pos = leaf_position;

        // Walk up tree from leaf to root, updating hashes and caching siblings
        for (level, empty_subtree_hash) in EMPTY_SUBTREE_HASHES.iter().enumerate() {
            // Calculate sibling position (flip last bit)
            let sibling_pos = current_pos ^ 1;

            // Get sibling hash from cache, or use empty subtree hash for this level
            let sibling_hash = self.level_caches[level]
                .get(&sibling_pos)
                .copied()
                .unwrap_or(*empty_subtree_hash);

            // Cache current hash before moving up
            self.level_caches[level].insert(current_pos, current_hash);

            // Compute parent hash based on position (left or right child)
            let bit = current_pos & 1;
            current_hash = if bit == 0 {
                // Left child: hash(current, sibling)
                Self::hash_combine(&current_hash, &sibling_hash)
            } else {
                // Right child: hash(sibling, current)
                Self::hash_combine(&sibling_hash, &current_hash)
            };

            // Move up to parent position
            current_pos /= 2;
        }

        self.current_root = current_hash;

        true
    }

    /// Check if a nonce exists in the SMT
    pub fn contains_nonce(&self, nonce: u64) -> bool {
        self.nonces.contains(&nonce)
    }

    /// Remove a nonce from the SMT (for rollback on transaction failure)
    ///
    /// This is used to keep the local SMT state in sync with on-chain state.
    /// When a transaction fails after inserting a nonce, we need to remove it
    /// to ensure the local SMT accurately reflects what's on-chain.
    ///
    /// Returns true if the nonce was removed, false if it didn't exist.
    pub fn remove_nonce(&mut self, nonce: u64) -> bool {
        self.nonces.remove(&nonce)
    }

    /// Get all nonces in the current tree
    pub fn get_nonces(&self) -> Vec<u64> {
        let mut nonces: Vec<u64> = self.nonces.iter().copied().collect();
        nonces.sort_unstable();
        nonces
    }

    /// Get the number of nonces in the current tree
    pub fn nonce_count(&self) -> usize {
        self.nonces.len()
    }

    /// Get the current SMT root hash
    pub fn current_root(&self) -> [u8; 32] {
        self.current_root
    }

    /// Generate exclusion proof for a nonce (before insertion)
    ///
    /// Returns array of 16 sibling hashes at the leaf position for a nonce
    /// that is NOT yet in the tree. This proves the nonce doesn't exist.
    ///
    /// Used for release_funds transactions where we prove:
    /// 1. Nonce doesn't exist (exclusion proof with current siblings)
    /// 2. After insertion, new root is computed
    ///
    /// Complexity: O(TREE_HEIGHT) = O(16) lookups
    pub fn generate_exclusion_proof(&self, nonce: u64) -> [[u8; 32]; TREE_HEIGHT] {
        // Calculate leaf position
        let leaf_position = nonce as usize % MAX_TREE_LEAVES;
        let mut sibling_proofs = [[0u8; 32]; TREE_HEIGHT];
        let mut current_pos = leaf_position;

        // Extract sibling hashes at each level from current tree state
        for (level, empty_subtree_hash) in EMPTY_SUBTREE_HASHES.iter().enumerate() {
            let sibling_pos = current_pos ^ 1; // Flip last bit

            // Get sibling hash from cache, or use empty subtree hash for this level
            let sibling_hash = self.level_caches[level]
                .get(&sibling_pos)
                .copied()
                .unwrap_or(*empty_subtree_hash);

            sibling_proofs[level] = sibling_hash;

            // Move up to parent position
            current_pos /= 2;
        }

        sibling_proofs
    }

    /// Reset the SMT state for a new tree index
    /// Called when the ResetSmtRoot instruction is processed on-chain
    pub fn reset(&mut self, new_tree_index: u64) {
        self.tree_index = new_tree_index;
        self.nonces.clear();
        self.current_root = EMPTY_TREE_ROOT;

        for cache in &mut self.level_caches {
            cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::{tree_constants::EMPTY_TREE_ROOT, EMPTY_LEAF, NON_EMPTY_LEAF_HASH};

    #[test]
    fn test_exclusion_proof_verification() {
        let mut state = SmtState::new(0);

        // Get exclusion proof BEFORE insertion
        let exclusion_proof = state.generate_exclusion_proof(42);
        let old_root = state.current_root();

        // Insert nonce
        assert!(state.insert_nonce(42));
        let new_root = state.current_root();

        // Verify exclusion: EMPTY_LEAF + siblings → old_root
        let leaf_position = 42_usize % MAX_TREE_LEAVES;
        let mut current_hash = EMPTY_LEAF;
        for (level, &sibling) in exclusion_proof.iter().enumerate() {
            let bit = (leaf_position >> level) & 1;
            current_hash = if bit == 0 {
                SmtState::hash_combine(&current_hash, &sibling)
            } else {
                SmtState::hash_combine(&sibling, &current_hash)
            };
        }
        assert_eq!(
            current_hash, old_root,
            "Exclusion proof failed: computed root doesn't match old root"
        );

        // Verify inclusion: NON_EMPTY_LEAF_HASH + SAME siblings → new_root
        let mut current_hash = NON_EMPTY_LEAF_HASH;
        for (level, &sibling) in exclusion_proof.iter().enumerate() {
            let bit = (leaf_position >> level) & 1;
            current_hash = if bit == 0 {
                SmtState::hash_combine(&current_hash, &sibling)
            } else {
                SmtState::hash_combine(&sibling, &current_hash)
            };
        }
        assert_eq!(
            current_hash, new_root,
            "Inclusion proof failed: computed root doesn't match new root"
        );
    }

    #[test]
    fn test_idempotent_insert() {
        let mut state = SmtState::new(0);

        assert!(state.insert_nonce(42));
        let root_after_first = state.current_root();

        // Duplicate insert should return false and not change root
        assert!(!state.insert_nonce(42));
        assert_eq!(state.current_root(), root_after_first);
        assert_eq!(state.nonce_count(), 1);
    }

    #[test]
    fn test_order_independence() {
        let mut state1 = SmtState::new(0);
        let mut state2 = SmtState::new(0);

        state1.insert_nonce(1);
        state1.insert_nonce(100);
        state1.insert_nonce(1000);

        state2.insert_nonce(1000);
        state2.insert_nonce(1);
        state2.insert_nonce(100);

        assert_eq!(state1.current_root(), state2.current_root());
    }

    #[test]
    fn test_reset() {
        let mut state = SmtState::new(0);
        state.insert_nonce(1);
        state.insert_nonce(2);

        let root_before = state.current_root();
        assert_ne!(root_before, EMPTY_TREE_ROOT);

        state.reset(5);

        assert_eq!(state.tree_index(), 5);
        assert_eq!(state.nonce_count(), 0);
        assert_eq!(state.current_root(), EMPTY_TREE_ROOT);

        // Can insert again after reset
        assert!(state.insert_nonce(1));
        assert_ne!(state.current_root(), EMPTY_TREE_ROOT);
    }

    #[test]
    fn test_multiple_inserts() {
        let mut state = SmtState::new(0);

        let nonces = vec![0, 1, 100, 1000, 5000, u64::MAX];
        for &nonce in &nonces {
            assert!(state.insert_nonce(nonce));
        }

        // All should be retrievable
        for &nonce in &nonces {
            assert!(state.contains_nonce(nonce));
        }

        assert_eq!(state.nonce_count(), nonces.len());
    }
}
