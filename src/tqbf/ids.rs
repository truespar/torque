//! IDS section (spec §5.3): FST from external id bytes to doc id, read in place.

use super::bytes::Bytes;
use std::io;

/// Build from external ids indexed by doc id. Ids must be unique.
pub fn build_section(external_ids: &[&str]) -> io::Result<Vec<u8>> {
    let mut pairs: Vec<(&str, u64)> = external_ids.iter().enumerate().map(|(i, id)| (*id, i as u64)).collect();
    pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    if let Some(w) = pairs.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("duplicate document id `{}`", w[0].0)));
    }
    let mut b = fst::MapBuilder::memory_sized(pairs.len());
    for (k, v) in pairs {
        b.insert(k, v).map_err(io::Error::other)?;
    }
    b.into_inner().map_err(io::Error::other)
}

pub struct IdMap(fst::Map<Bytes>);

impl IdMap {
    pub fn open(section: Bytes) -> io::Result<Self> {
        fst::Map::new(section).map(Self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    #[inline]
    pub fn get(&self, external_id: &str) -> Option<u32> {
        self.0.get(external_id).map(|v| v as u32)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The FST of external id to doc id.
    pub fn map(&self) -> &fst::Map<Bytes> {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_duplicates() {
        let ids = ["7405355", "12", "5565651", "abc"];
        let map = IdMap::open(Bytes::from_vec(build_section(&ids).unwrap())).unwrap();
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(map.get(id), Some(i as u32));
        }
        assert_eq!(map.get("nope"), None);
        assert!(build_section(&["a", "b", "a"]).is_err());
    }

    /// A builder sized to its keys (the vendored fst's `memory_sized`) builds the map the default
    /// builder does, whatever the hint: below the keys, at them, above them.
    #[test]
    fn sized_builders_build_the_same_map() {
        let mut keys: Vec<String> = (0..3000u64).map(|i| format!("{}-{}", i * 7919 % 10007, ["bil", "båt", "hus"][(i % 3) as usize])).collect();
        keys.sort();
        keys.dedup();
        let build = |mut b: fst::MapBuilder<Vec<u8>>| {
            for (i, k) in keys.iter().enumerate() {
                b.insert(k, (i * 31) as u64).unwrap();
            }
            fst::Map::new(b.into_inner().unwrap()).unwrap()
        };
        let all = |m: &fst::Map<Vec<u8>>| {
            use fst::Streamer;
            let mut out = Vec::new();
            let mut s = m.stream();
            while let Some((k, v)) = s.next() {
                out.push((k.to_vec(), v));
            }
            out
        };
        let full = all(&build(fst::MapBuilder::memory()));
        assert_eq!(full.len(), keys.len());
        for hint in [0usize, 1, 10, 500, keys.len(), 50_000] {
            let sized = build(fst::MapBuilder::memory_sized(hint));
            assert_eq!(all(&sized), full, "hint {hint}");
            assert_eq!(sized.get(&keys[1234]), Some(1234 * 31));
        }
    }
}
