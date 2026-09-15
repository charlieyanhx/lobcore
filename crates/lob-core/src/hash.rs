//! Book-state hash: sha256 over bid side then ask side, ascending price, each level as
//! `i64 px | u32 count` followed by `(u64 id | u32 qty)` in FIFO order, all little-endian, no
//! separators and no version byte (the contract in docs/PLAN.md, reproduced by the Python
//! reference model with `hashlib`).

use sha2::{Digest, Sha256};

use crate::types::{OrderId, Px, Qty};

/// Incremental state hasher; feed levels in the contract order.
pub struct StateHasher {
    h: Sha256,
}

impl std::fmt::Debug for StateHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StateHasher")
    }
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHasher {
    pub fn new() -> StateHasher {
        StateHasher { h: Sha256::new() }
    }

    #[inline]
    pub fn level(&mut self, px: Px, count: u32) {
        self.h.update((px as i64).to_le_bytes());
        self.h.update(count.to_le_bytes());
    }

    #[inline]
    pub fn order(&mut self, id: OrderId, qty: Qty) {
        self.h.update(id.to_le_bytes());
        self.h.update(qty.to_le_bytes());
    }

    pub fn finish(self) -> [u8; 32] {
        self.h.finalize().into()
    }
}

/// Hash a snapshot-shaped view: `(px, [(id, qty)])` levels already in contract order.
pub fn hash_levels<'a, I, J>(levels: I) -> [u8; 32]
where
    I: IntoIterator<Item = (Px, J)>,
    J: IntoIterator<Item = &'a (OrderId, Qty)>,
{
    let mut h = StateHasher::new();
    for (px, fifo) in levels {
        let fifo: Vec<&(OrderId, Qty)> = fifo.into_iter().collect();
        h.level(px, fifo.len() as u32);
        for &(id, qty) in fifo {
            h.order(id, qty);
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::hex;

    #[test]
    fn empty_book_hash_is_sha256_of_nothing() {
        // hashlib.sha256(b"").hexdigest()
        assert_eq!(
            hex(&StateHasher::new().finish()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn one_level_hash_matches_hashlib() {
        // python: hashlib.sha256(struct.pack("<qI", 10050, 2) + struct.pack("<QI", 7, 100)
        //                        + struct.pack("<QI", 8, 50)).hexdigest()
        let mut h = StateHasher::new();
        h.level(10050, 2);
        h.order(7, 100);
        h.order(8, 50);
        assert_eq!(
            hex(&h.finish()),
            "4e8c54964667ea21186a0288a0f66cb8f73d203cf79e65f27817296aea896ce3"
        );
    }
}
