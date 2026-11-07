//! Byte-string interner for the builder: every distinct string once in one arena, numbered in
//! first-seen order, found again through an open-addressing table of ids. No allocation per
//! string; about 16 bytes per entry plus the bytes themselves.

const EMPTY: u32 = u32::MAX;

pub(crate) struct Interner {
    arena: Vec<u8>,
    /// Start of entry `i` in the arena; `starts[i + 1]` is its end.
    starts: Vec<u32>,
    hashes: Vec<u32>,
    /// Entry ids, `EMPTY` for a free slot; length a power of two, at most half full.
    slots: Vec<u32>,
}

#[inline]
fn hash(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    h.write(bytes);
    // FxHash leaves the low bits weak for short keys; fold the high half in.
    let v = h.finish();
    v ^ (v >> 29)
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Self {
        Self { arena: Vec::new(), starts: vec![0], hashes: Vec::new(), slots: vec![EMPTY; 16] }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    #[inline]
    pub fn get(&self, id: u32) -> &[u8] {
        &self.arena[self.starts[id as usize] as usize..self.starts[id as usize + 1] as usize]
    }

    /// Bytes held: arena plus tables.
    pub fn heap_bytes(&self) -> usize {
        self.arena.capacity() + 4 * (self.starts.capacity() + self.hashes.capacity() + self.slots.capacity())
    }

    /// Id of `bytes`, and whether it was added now.
    #[inline]
    pub fn intern(&mut self, bytes: &[u8]) -> (u32, bool) {
        let h = hash(bytes);
        let mask = self.slots.len() - 1;
        let mut i = (h as usize) & mask;
        loop {
            let id = self.slots[i];
            if id == EMPTY {
                break;
            }
            if self.hashes[id as usize] == h as u32 && self.get(id) == bytes {
                return (id, false);
            }
            i = (i + 1) & mask;
        }
        let id = self.hashes.len() as u32;
        self.arena.extend_from_slice(bytes);
        self.starts.push(self.arena.len() as u32);
        self.hashes.push(h as u32);
        self.slots[i] = id;
        if self.hashes.len() * 2 > self.slots.len() {
            self.grow();
        }
        (id, true)
    }

    /// Id of `bytes` when interned.
    pub fn find(&self, bytes: &[u8]) -> Option<u32> {
        let h = hash(bytes);
        let mask = self.slots.len() - 1;
        let mut i = (h as usize) & mask;
        loop {
            let id = self.slots[i];
            if id == EMPTY {
                return None;
            }
            if self.hashes[id as usize] == h as u32 && self.get(id) == bytes {
                return Some(id);
            }
            i = (i + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let size = self.slots.len() * 2;
        let mask = size - 1;
        let mut slots = vec![EMPTY; size];
        for (id, &h) in self.hashes.iter().enumerate() {
            // The stored hash keeps only the low 32 bits; the probe start needs no more.
            let mut i = (h as usize) & mask;
            while slots[i] != EMPTY {
                i = (i + 1) & mask;
            }
            slots[i] = id as u32;
        }
        self.slots = slots;
    }

    /// Ids in byte order of their strings.
    pub fn sorted_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = (0..self.len() as u32).collect();
        ids.sort_unstable_by(|&a, &b| self.get(a).cmp(self.get(b)));
        ids
    }

    /// The arena and the ends of the entries, the strings in the order of `ids`.
    pub fn reordered(&self, ids: &[u32]) -> (Vec<u8>, Vec<u32>) {
        let mut arena = Vec::with_capacity(self.arena.len());
        let mut starts = Vec::with_capacity(ids.len() + 1);
        starts.push(0u32);
        for &id in ids {
            arena.extend_from_slice(self.get(id));
            starts.push(arena.len() as u32);
        }
        (arena, starts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_finds_and_sorts() {
        let mut t = Interner::new();
        let words: Vec<String> = (0..10_000).map(|i| format!("w{}", i * 7919 % 10_007)).collect();
        for (i, w) in words.iter().enumerate() {
            assert_eq!(t.intern(w.as_bytes()), (i as u32, true));
        }
        for (i, w) in words.iter().enumerate() {
            assert_eq!(t.intern(w.as_bytes()), (i as u32, false));
            assert_eq!(t.find(w.as_bytes()), Some(i as u32));
        }
        assert_eq!(t.find(b"nope"), None);
        assert_eq!(t.intern(b""), (10_000, true));
        let ids = t.sorted_ids();
        let (arena, starts) = t.reordered(&ids);
        let sorted: Vec<&[u8]> = starts.windows(2).map(|w| &arena[w[0] as usize..w[1] as usize]).collect();
        let mut want: Vec<&[u8]> = words.iter().map(|w| w.as_bytes()).chain([&b""[..]]).collect();
        want.sort();
        assert_eq!(sorted, want);
    }
}
