use const_crypto::sha2::Sha256;
use std::collections::HashSet;

pub const EMPTY_TREE_ROOT: [u8; 32] = [
    143, 230, 177, 104, 146, 86, 192, 211, 133, 244, 47, 91, 190, 32, 39, 162, 44, 25, 150, 225,
    16, 186, 151, 193, 113, 211, 229, 148, 141, 233, 43, 235,
];

const TREE_HEIGHT: usize = 16;
pub const MAX_TREE_LEAVES: usize = 2_usize.pow(16);

/// Simple SMT using complete binary tree
pub struct ProcessorSMT {
    /// All the leaves - 65,536 positions
    leaves: Vec<[u8; 32]>,
    /// Track which nonces we've inserted
    used_nonces: HashSet<u64>,
}

impl Default for ProcessorSMT {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorSMT {
    pub fn new() -> Self {
        Self {
            leaves: vec![[0u8; 32]; MAX_TREE_LEAVES], // All empty initially
            used_nonces: HashSet::new(),
        }
    }

    pub fn insert(&mut self, nonce: u64) {
        if self.used_nonces.contains(&nonce) {
            return; // Already inserted
        }

        self.used_nonces.insert(nonce);

        // Find where this nonce goes in the 65k array
        let position = nonce as usize % MAX_TREE_LEAVES;

        // Mark this position as occupied with non-zero hash
        self.leaves[position] = Self::safe_sha256(&[1u8; 32]);
    }

    pub fn contains(&self, nonce: u64) -> bool {
        self.used_nonces.contains(&nonce)
    }

    /// Get current SMT root by building complete tree
    pub fn current_root(&self) -> [u8; 32] {
        self.build_tree_and_get_root()
    }

    /// Build the complete binary tree bottom-up and return root
    fn build_tree_and_get_root(&self) -> [u8; 32] {
        let mut current_level = self.leaves.clone(); // Start with 65k leaves

        // Build tree level by level
        for _level in 0..TREE_HEIGHT {
            let mut next_level = Vec::new();

            // Hash pairs to create next level up
            for i in (0..current_level.len()).step_by(2) {
                let left = current_level[i];
                let right = current_level[i + 1];
                let parent = Self::hash_combine(&left, &right);
                next_level.push(parent);
            }

            current_level = next_level;
        }

        // Should have exactly one node left - the root
        assert_eq!(current_level.len(), 1);
        current_level[0]
    }

    /// Generate exclusion proof for a nonce
    pub fn generate_exclusion_proof_for_verification(&self, nonce: u64) -> ([u8; 32], [u8; 512]) {
        if self.used_nonces.contains(&nonce) {
            panic!("Cannot generate exclusion proof for existing nonce");
        }

        let current_root = self.current_root();
        let sibling_proofs = self.extract_sibling_proofs(nonce);

        // Convert to flat byte array
        let mut sibling_bytes = [0u8; 512];
        for (level, &sibling) in sibling_proofs.iter().enumerate() {
            let start_idx = level * 32;
            sibling_bytes[start_idx..start_idx + 32].copy_from_slice(&sibling);
        }

        (current_root, sibling_bytes)
    }

    pub fn generate_exclusion_proof(&self, nonce: u64) -> ([u8; 32], [u8; 512]) {
        self.generate_exclusion_proof_for_verification(nonce)
    }

    /// Extract sibling proofs by building tree and looking up siblings
    fn extract_sibling_proofs(&self, nonce: u64) -> [[u8; 32]; TREE_HEIGHT] {
        let position = nonce as usize % MAX_TREE_LEAVES;

        let mut siblings = [[0u8; 32]; TREE_HEIGHT];
        let mut current_level = self.leaves.clone();
        let mut current_pos = position;

        // Extract sibling at each level as we build tree
        for sibling in siblings.iter_mut() {
            // Find sibling position (flip the last bit)
            let sibling_pos = current_pos ^ 1;
            *sibling = current_level[sibling_pos];

            // Build next level up
            let mut next_level = Vec::new();
            for i in (0..current_level.len()).step_by(2) {
                let left = current_level[i];
                let right = current_level[i + 1];
                let parent = Self::hash_combine(&left, &right);
                next_level.push(parent);
            }

            current_level = next_level;
            current_pos /= 2; // Move up to parent position
        }

        siblings
    }

    fn hash_combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(left);
        combined[32..].copy_from_slice(right);
        Self::safe_sha256(&combined)
    }

    fn safe_sha256(input: &[u8]) -> [u8; 32] {
        Sha256::new().update(input).finalize()
    }
}

impl Clone for ProcessorSMT {
    fn clone(&self) -> Self {
        Self {
            leaves: self.leaves.clone(),
            used_nonces: self.used_nonces.clone(),
        }
    }
}
