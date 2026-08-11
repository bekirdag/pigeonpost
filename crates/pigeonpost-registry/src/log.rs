//! The transparency log.
//!
//! RFC 6962's Merkle tree, unchanged, because that is what existing witness tooling verifies —
//! `docs/architecture.md` picks the C2SP `tlog-witness` / Sigsum model precisely so a witness is a
//! cron job someone can already run, not a bespoke client we have to ship.
//!
//! Two proofs matter:
//!
//! - **inclusion** — "my name is in the log the world sees". Returned with every resolve.
//! - **consistency** — "the log only ever appended". This is what each witness checks before
//!   cosigning. Preventing split views without gossip additionally requires accepted quorums to
//!   intersect in at least one non-equivocating witness; Merkle consistency alone cannot establish
//!   that operational assumption.
//!
//! Domain-separated leaf and node prefixes (`0x00` / `0x01`) stop a second-preimage attack where
//! an internal node is passed off as a leaf.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub type Hash = [u8; 32];

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

/// Hash of an empty tree: `SHA-256("")`, per RFC 6962.
pub fn empty_root() -> Hash {
    Sha256::digest([]).into()
}

pub fn leaf_hash(data: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_PREFIX]);
    hasher.update(data);
    hasher.finalize().into()
}

pub(crate) fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([NODE_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

pub(crate) fn hash_eq(left: &Hash, right: &Hash) -> bool {
    bool::from(left.ct_eq(right))
}

/// Largest power of two strictly less than `n`. RFC 6962's split point.
pub(crate) fn split(n: usize) -> usize {
    debug_assert!(n > 1);
    let mut k = 1;
    while k << 1 < n {
        k <<= 1;
    }
    k
}

/// An append-only log of leaf hashes.
///
/// Holds hashes, not entries: 32 bytes per name means a million handles is 32 MB, so the whole
/// tree stays in memory on the one small box `docs/capacity.md` budgets for the registry.
#[derive(Debug, Default, Clone)]
pub struct MerkleLog {
    leaves: Vec<Hash>,
}

/// Compact append state for independently auditing a growing RFC 6962 log.
///
/// One hash is retained for each set bit in `size`, so storage is bounded by 64 hashes even for a
/// registry with billions of entries. A client persists this only after recomputing the advertised
/// checkpoint root; on the next refresh it can authenticate every newly appended leaf without
/// downloading the historical log again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MerkleFrontier {
    size: u64,
    peaks: Vec<Option<Hash>>,
}

impl Default for MerkleFrontier {
    fn default() -> Self {
        Self::new()
    }
}

impl MerkleFrontier {
    pub fn new() -> Self {
        Self {
            size: 0,
            peaks: Vec::new(),
        }
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Reject malformed persisted state before it becomes a trust anchor.
    pub fn validate(&self) -> bool {
        self.peaks.len() <= u64::BITS as usize
            && self.peaks.iter().enumerate().all(|(level, peak)| {
                let occupied = ((self.size >> level) & 1) == 1;
                peak.is_some() == occupied
            })
            && (self.peaks.len() == u64::BITS as usize || self.size >> self.peaks.len() == 0)
    }

    /// Append one already domain-separated leaf hash. Returns its zero-based index.
    pub fn append_hash(&mut self, leaf: Hash) -> Option<u64> {
        if !self.validate() || self.size == u64::MAX {
            return None;
        }
        let index = self.size;
        let mut level = 0usize;
        let mut hash = leaf;
        let mut occupied = self.size;
        while occupied & 1 == 1 {
            let left = self.peaks.get_mut(level)?.take()?;
            hash = node_hash(&left, &hash);
            occupied >>= 1;
            level += 1;
        }
        if self.peaks.len() <= level {
            self.peaks.resize(level + 1, None);
        }
        self.peaks[level] = Some(hash);
        self.size += 1;
        Some(index)
    }

    pub fn append(&mut self, data: &[u8]) -> Option<u64> {
        self.append_hash(leaf_hash(data))
    }

    /// The RFC 6962 root represented by this frontier.
    pub fn root(&self) -> Option<Hash> {
        if !self.validate() {
            return None;
        }
        if self.size == 0 {
            return Some(empty_root());
        }
        let mut root: Option<Hash> = None;
        for peak in self.peaks.iter().flatten() {
            root = Some(match root {
                Some(right) => node_hash(peak, &right),
                None => *peak,
            });
        }
        root
    }
}

impl MerkleLog {
    pub fn new() -> Self {
        MerkleLog { leaves: Vec::new() }
    }

    pub fn from_leaves(leaves: Vec<Hash>) -> Self {
        MerkleLog { leaves }
    }

    /// Append an entry, returning its index.
    pub fn append(&mut self, data: &[u8]) -> u64 {
        self.leaves.push(leaf_hash(data));
        (self.leaves.len() - 1) as u64
    }

    pub fn size(&self) -> u64 {
        self.leaves.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    pub fn root(&self) -> Hash {
        self.root_of(self.leaves.len())
    }

    /// Root of the first `n` leaves — needed for consistency proofs against an older head.
    fn root_of(&self, n: usize) -> Hash {
        if n == 0 {
            return empty_root();
        }
        Self::root_range(&self.leaves[..n])
    }

    fn root_range(leaves: &[Hash]) -> Hash {
        match leaves.len() {
            0 => empty_root(),
            1 => leaves[0],
            n => {
                let k = split(n);
                node_hash(
                    &Self::root_range(&leaves[..k]),
                    &Self::root_range(&leaves[k..]),
                )
            }
        }
    }

    /// Proof that leaf `index` is in the tree of size `size`.
    pub fn inclusion_proof(&self, index: u64, size: u64) -> Option<Vec<Hash>> {
        let (index, size) = (index as usize, size as usize);
        if size > self.leaves.len() || index >= size {
            return None;
        }
        Some(Self::path(&self.leaves[..size], index))
    }

    fn path(leaves: &[Hash], index: usize) -> Vec<Hash> {
        let n = leaves.len();
        if n <= 1 {
            return Vec::new();
        }
        let k = split(n);
        if index < k {
            let mut proof = Self::path(&leaves[..k], index);
            proof.push(Self::root_range(&leaves[k..]));
            proof
        } else {
            let mut proof = Self::path(&leaves[k..], index - k);
            proof.push(Self::root_range(&leaves[..k]));
            proof
        }
    }

    /// Proof that the tree of size `old` is a prefix of the tree of size `new`.
    pub fn consistency_proof(&self, old: u64, new: u64) -> Option<Vec<Hash>> {
        let (old, new) = (old as usize, new as usize);
        if old > new || new > self.leaves.len() || old == 0 {
            return None;
        }
        Some(Self::consistency(&self.leaves[..new], old, true))
    }

    fn consistency(leaves: &[Hash], m: usize, is_root: bool) -> Vec<Hash> {
        let n = leaves.len();
        if m == n {
            // The old tree is exactly this subtree; only include its root when it is not already
            // implied by the caller's own head.
            return if is_root {
                Vec::new()
            } else {
                vec![Self::root_range(leaves)]
            };
        }

        let k = split(n);
        if m <= k {
            let mut proof = Self::consistency(&leaves[..k], m, is_root);
            proof.push(Self::root_range(&leaves[k..]));
            proof
        } else {
            let mut proof = Self::consistency(&leaves[k..], m - k, false);
            proof.push(Self::root_range(&leaves[..k]));
            proof
        }
    }
}

/// Recompute a root from a leaf and an inclusion proof.
///
/// This is what a *client* runs — it never sees the tree, only the leaf it cares about and a few
/// KB of hashes. Verification here is why serving the log confers no authority.
pub fn verify_inclusion(
    leaf: &Hash,
    index: u64,
    size: u64,
    proof: &[Hash],
    expected_root: &Hash,
) -> bool {
    if index >= size {
        return false;
    }

    // Bottom-up, matching the order `path` emits: the first hash is the sibling nearest the leaf.
    // `node` tracks our position and `last` the final leaf, both shifted right one level at a
    // time; an odd position means we are a right child, so the sibling goes on the left.
    let (mut node, mut last) = (index, size - 1);
    let mut hash = *leaf;

    for sibling in proof {
        if last == 0 {
            return false; // more proof than the tree has levels
        }
        if node % 2 == 1 || node == last {
            hash = node_hash(sibling, &hash);
            while node != 0 && node % 2 == 0 {
                node /= 2;
                last /= 2;
            }
        } else {
            hash = node_hash(&hash, sibling);
        }
        node /= 2;
        last /= 2;
    }

    last == 0 && hash_eq(&hash, expected_root)
}

/// Verify that `old_root` (size `old`) is a prefix of `new_root` (size `new`).
pub fn verify_consistency(
    old: u64,
    old_root: &Hash,
    new: u64,
    new_root: &Hash,
    proof: &[Hash],
) -> bool {
    if old == 0 || old > new {
        return false;
    }
    if old == new {
        return proof.is_empty() && hash_eq(old_root, new_root);
    }

    // RFC 6962 §2.1.2. Rebuild the old root and the new root from the same proof: if both come
    // out right, every entry the old head covered is still there, unchanged, in the same order.
    let mut proof = proof.to_vec();

    // When the old size is a perfect subtree its root is omitted from the proof, because the
    // verifier already holds it.
    if old.is_power_of_two() {
        proof.insert(0, *old_root);
    }

    let mut iter = proof.iter();
    let Some(first) = iter.next() else {
        return false;
    };
    let (mut old_hash, mut new_hash) = (*first, *first);

    let (mut node, mut last) = (old - 1, new - 1);
    while node % 2 == 1 {
        node /= 2;
        last /= 2;
    }

    for step in iter {
        if last == 0 {
            return false;
        }
        if node % 2 == 1 || node == last {
            old_hash = node_hash(step, &old_hash);
            new_hash = node_hash(step, &new_hash);
            while node != 0 && node % 2 == 0 {
                node /= 2;
                last /= 2;
            }
        } else {
            new_hash = node_hash(&new_hash, step);
        }
        node /= 2;
        last /= 2;
    }

    last == 0 && hash_eq(&old_hash, old_root) && hash_eq(&new_hash, new_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_frontier_matches_full_tree_across_growth() {
        let mut full = MerkleLog::new();
        let mut compact = MerkleFrontier::new();
        assert_eq!(compact.root(), Some(full.root()));
        for index in 0..10_000u64 {
            let leaf = format!("entry-{index}");
            assert_eq!(compact.append(leaf.as_bytes()), Some(index));
            full.append(leaf.as_bytes());
            assert!(compact.validate());
            assert_eq!(compact.size(), full.size());

            // The reference tree recomputes its root recursively, so comparing on
            // every append makes this regression test quadratic. Keep exhaustive
            // coverage while the tree is small, then sample structural boundaries
            // and the final 10,000-entry state.
            let size = index + 1;
            if size <= 1_024 || size.is_power_of_two() || size % 997 == 0 || size == 10_000 {
                assert_eq!(compact.root(), Some(full.root()));
            }
        }
        assert!(compact.peaks.len() <= u64::BITS as usize);
    }

    #[test]
    fn malformed_persisted_frontier_is_rejected() {
        let malformed = MerkleFrontier {
            size: 1,
            peaks: vec![None],
        };
        assert!(!malformed.validate());
        assert_eq!(malformed.root(), None);
    }

    fn log_of(n: usize) -> MerkleLog {
        let mut log = MerkleLog::new();
        for i in 0..n {
            log.append(format!("entry {i}").as_bytes());
        }
        log
    }

    #[test]
    fn an_empty_log_has_the_rfc_6962_empty_root() {
        // SHA-256 of the empty string, so an empty log is not a special case anyone has to code.
        assert_eq!(MerkleLog::new().root(), empty_root());
        assert_eq!(
            hex(&empty_root()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_single_leaf_tree_is_its_leaf_hash() {
        let mut log = MerkleLog::new();
        log.append(b"only");
        assert_eq!(log.root(), leaf_hash(b"only"));
    }

    #[test]
    fn appending_changes_the_root() {
        let mut log = log_of(3);
        let before = log.root();
        log.append(b"another");
        assert_ne!(log.root(), before);
    }

    #[test]
    fn inclusion_proofs_verify_for_every_leaf_at_every_size() {
        for size in 1..=33usize {
            let log = log_of(size);
            let root = log.root();
            for index in 0..size {
                let proof = log.inclusion_proof(index as u64, size as u64).unwrap();
                assert!(
                    verify_inclusion(
                        &leaf_hash(format!("entry {index}").as_bytes()),
                        index as u64,
                        size as u64,
                        &proof,
                        &root
                    ),
                    "size {size}, index {index}"
                );
            }
        }
    }

    #[test]
    fn an_inclusion_proof_for_the_wrong_leaf_fails() {
        let log = log_of(8);
        let proof = log.inclusion_proof(3, 8).unwrap();
        assert!(!verify_inclusion(
            &leaf_hash(b"not in the log"),
            3,
            8,
            &proof,
            &log.root()
        ));
    }

    #[test]
    fn a_tampered_inclusion_proof_fails() {
        let log = log_of(8);
        let mut proof = log.inclusion_proof(3, 8).unwrap();
        proof[0][0] ^= 0xff;
        assert!(!verify_inclusion(
            &leaf_hash(b"entry 3"),
            3,
            8,
            &proof,
            &log.root()
        ));
    }

    #[test]
    fn proof_sizes_stay_logarithmic() {
        // The claim in architecture.md is that a log of 80M entries needs ~3 KB of proof.
        // log2(80M) is about 27 hashes, which is 864 bytes — comfortably inside that.
        let log = log_of(1024);
        let proof = log.inclusion_proof(500, 1024).unwrap();
        assert_eq!(proof.len(), 10, "log2(1024)");
    }

    #[test]
    fn consistency_proofs_verify_across_every_pair_of_sizes() {
        for new in 1..=17u64 {
            let log = log_of(new as usize);
            let new_root = log.root();
            for old in 1..=new {
                let old_root = log_of(old as usize).root();
                let proof = log.consistency_proof(old, new).unwrap();
                assert!(
                    verify_consistency(old, &old_root, new, &new_root, &proof),
                    "old {old}, new {new}"
                );
            }
        }
    }

    #[test]
    fn a_rewritten_log_fails_its_consistency_proof() {
        // The attack the whole design exists to catch: the operator changes history.
        let honest = log_of(8);
        let old_root = log_of(4).root();

        let mut rewritten = log_of(3);
        rewritten.append(b"forged entry 3");
        for i in 4..8 {
            rewritten.append(format!("entry {i}").as_bytes());
        }

        let proof = rewritten.consistency_proof(4, 8).unwrap();
        assert!(
            !verify_consistency(4, &old_root, 8, &rewritten.root(), &proof),
            "a witness must be able to prove the log was rewritten"
        );
        assert_ne!(honest.root(), rewritten.root());
    }

    #[test]
    fn proofs_outside_the_tree_are_refused() {
        let log = log_of(4);
        assert!(log.inclusion_proof(4, 4).is_none());
        assert!(log.inclusion_proof(0, 5).is_none());
        assert!(log.consistency_proof(5, 4).is_none());
        assert!(log.consistency_proof(0, 4).is_none(), "size 0 has no root");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
