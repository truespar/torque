use crate::bitmap::Bitmap;
use rustc_hash::FxHashMap;

/// Contiguous flat bitmap storage for GPU transfer.
/// All bitmaps have the same width (words_per_bitmap) and are stored
/// back-to-back in a single Vec<u64>.
pub struct FlatBitmapStore {
    /// Contiguous bitmap data: bitmap[i] occupies data[i*words_per_bitmap..(i+1)*words_per_bitmap]
    pub data: Vec<u64>,
    /// Number of u64 words per bitmap: ceil(total_docs / 64)
    pub words_per_bitmap: usize,
    /// Number of bitmaps stored
    pub num_bitmaps: usize,
}

impl FlatBitmapStore {
    pub fn new(total_docs: u32) -> Self {
        let words_per_bitmap = (total_docs as usize + 63) / 64;
        Self {
            data: Vec::new(),
            words_per_bitmap,
            num_bitmaps: 0,
        }
    }

    /// Add a Bitmap as a new flat bitmap. Returns the dense bitmap ID (index).
    pub fn add_bitmap(&mut self, bitmap: &Bitmap) -> u32 {
        let id = self.num_bitmaps as u32;
        let start = self.data.len();
        self.data.resize(start + self.words_per_bitmap, 0u64);

        for doc_id in bitmap.iter() {
            let word_idx = doc_id as usize / 64;
            let bit_pos = doc_id as usize % 64;
            if word_idx < self.words_per_bitmap {
                self.data[start + word_idx] |= 1u64 << bit_pos;
            }
        }

        self.num_bitmaps += 1;
        id
    }

    /// Read back a flat bitmap as a Bitmap (CPU path).
    pub fn get_bitmap(&self, id: u32) -> Bitmap {
        let offset = id as usize * self.words_per_bitmap;
        let mut result = Bitmap::new();

        for w in 0..self.words_per_bitmap {
            let word = self.data[offset + w];
            if word == 0 {
                continue;
            }
            let base = (w * 64) as u32;
            let mut bits = word;
            while bits != 0 {
                let bit_pos = bits.trailing_zeros();
                result.add(base + bit_pos);
                bits &= bits - 1; // clear lowest set bit
            }
        }

        result
    }

    /// CPU fallback: intersect two flat bitmaps, returning a Bitmap.
    pub fn intersect_cpu(&self, a: u32, b: u32) -> Bitmap {
        let offset_a = a as usize * self.words_per_bitmap;
        let offset_b = b as usize * self.words_per_bitmap;
        let mut result = Bitmap::new();

        for w in 0..self.words_per_bitmap {
            let word = self.data[offset_a + w] & self.data[offset_b + w];
            if word == 0 {
                continue;
            }
            let base = (w * 64) as u32;
            let mut bits = word;
            while bits != 0 {
                let bit_pos = bits.trailing_zeros();
                result.add(base + bit_pos);
                bits &= bits - 1;
            }
        }

        result
    }

    /// Get the byte offset of a bitmap within the data array.
    pub fn bitmap_word_offset(&self, id: u32) -> usize {
        id as usize * self.words_per_bitmap
    }

    /// Total bytes used by all bitmap data.
    pub fn data_bytes(&self) -> usize {
        self.data.len() * std::mem::size_of::<u64>()
    }
}

/// Reference to a bitmap lookup result.
pub enum BitmapRef<'a> {
    /// Dense bitmap stored in FlatBitmapStore (index into store).
    Dense(u32),
    /// Sparse bitmap stored as Bitmap.
    Sparse(&'a Bitmap),
}

/// Hybrid bitmap store: dense bitmaps for high-frequency terms (GPU-friendly),
/// sparse Bitmaps for low-frequency terms (CPU).
pub struct HybridBitmapStore {
    pub dense: FlatBitmapStore,
    /// term → dense bitmap index
    pub dense_terms: FxHashMap<String, u32>,
    /// term → sparse Bitmap
    pub sparse: FxHashMap<String, Bitmap>,
    /// Minimum doc_freq for a term to be stored as dense
    pub density_threshold: u32,
}

impl HybridBitmapStore {
    /// Build a HybridBitmapStore from a map of term→bitmap.
    /// Terms with doc_freq >= threshold go dense; the rest stay sparse.
    /// `vram_budget_bytes` caps the total dense bitmap memory.
    pub fn build(
        terms: FxHashMap<String, Bitmap>,
        total_docs: u32,
        vram_budget_bytes: usize,
    ) -> Self {
        let words_per_bitmap = (total_docs as usize + 63) / 64;
        let bytes_per_bitmap = words_per_bitmap * 8;

        // Sort terms by doc frequency (descending) to prioritize densest terms
        let mut term_list: Vec<(String, Bitmap)> = terms.into_iter().collect();
        term_list.sort_unstable_by(|a, b| b.1.cardinality().cmp(&a.1.cardinality()));

        // Calculate how many dense bitmaps fit in VRAM budget
        let max_dense = if bytes_per_bitmap > 0 {
            vram_budget_bytes / bytes_per_bitmap
        } else {
            0
        };

        // Split into dense vs sparse
        let split_point = term_list
            .iter()
            .position(|(_term, bm)| bm.is_empty())
            .unwrap_or(term_list.len())
            .min(max_dense);
        let sparse_list = term_list.split_off(split_point);
        let dense_list = term_list; // first split_point entries

        let density_threshold = dense_list
            .last()
            .map(|(_, bm)| bm.cardinality() as u32)
            .unwrap_or(0);

        // Pre-allocate flat bitmap storage for all dense bitmaps at once
        let num_dense = dense_list.len();
        let flat_data = vec![0u64; num_dense * words_per_bitmap];

        // Convert Bitmaps to flat bitmaps in parallel
        use rayon::prelude::*;
        dense_list
            .par_iter()
            .enumerate()
            .for_each(|(i, (_term, bitmap))| {
                let start = i * words_per_bitmap;
                // Safety: each thread writes to a disjoint slice
                let slice = unsafe {
                    std::slice::from_raw_parts_mut(
                        flat_data.as_ptr().add(start) as *mut u64,
                        words_per_bitmap,
                    )
                };
                for doc_id in bitmap.iter() {
                    let word_idx = doc_id as usize / 64;
                    let bit_pos = doc_id as usize % 64;
                    if word_idx < words_per_bitmap {
                        slice[word_idx] |= 1u64 << bit_pos;
                    }
                }
            });

        let dense_terms: FxHashMap<String, u32> = dense_list
            .into_iter()
            .enumerate()
            .map(|(i, (term, _))| (term, i as u32))
            .collect();

        let sparse: FxHashMap<String, Bitmap> = sparse_list.into_iter().collect();

        Self {
            dense: FlatBitmapStore {
                data: flat_data,
                words_per_bitmap,
                num_bitmaps: num_dense,
            },
            dense_terms,
            sparse,
            density_threshold,
        }
    }

    /// Look up a term, returning either a dense bitmap ID or a sparse reference.
    pub fn lookup(&self, term: &str) -> Option<BitmapRef<'_>> {
        if let Some(&id) = self.dense_terms.get(term) {
            return Some(BitmapRef::Dense(id));
        }
        self.sparse.get(term).map(BitmapRef::Sparse)
    }

    /// Total number of terms stored (dense + sparse).
    pub fn total_terms(&self) -> usize {
        self.dense_terms.len() + self.sparse.len()
    }

    /// Number of dense terms (GPU-resident).
    pub fn dense_count(&self) -> usize {
        self.dense_terms.len()
    }

    /// Number of sparse terms (CPU-only).
    pub fn sparse_count(&self) -> usize {
        self.sparse.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_bitmap_store_basic() {
        let mut store = FlatBitmapStore::new(128);
        assert_eq!(store.words_per_bitmap, 2); // 128/64 = 2

        let mut bm = Bitmap::new();
        bm.add(0);
        bm.add(63);
        bm.add(64);
        bm.add(127);

        let id = store.add_bitmap(&bm);
        assert_eq!(id, 0);
        assert_eq!(store.num_bitmaps, 1);

        let readback = store.get_bitmap(0);
        assert_eq!(readback, bm);
    }

    #[test]
    fn flat_bitmap_store_multiple() {
        let mut store = FlatBitmapStore::new(64);

        let mut bm_a = Bitmap::new();
        bm_a.add_range(0..32);

        let mut bm_b = Bitmap::new();
        bm_b.add_range(16..48);

        let id_a = store.add_bitmap(&bm_a);
        let id_b = store.add_bitmap(&bm_b);

        assert_eq!(id_a, 0);
        assert_eq!(id_b, 1);
        assert_eq!(store.get_bitmap(0), bm_a);
        assert_eq!(store.get_bitmap(1), bm_b);
    }

    #[test]
    fn flat_bitmap_intersect_cpu() {
        let mut store = FlatBitmapStore::new(128);

        let mut bm_a = Bitmap::new();
        bm_a.add_range(0..64);

        let mut bm_b = Bitmap::new();
        bm_b.add_range(32..96);

        store.add_bitmap(&bm_a);
        store.add_bitmap(&bm_b);

        let result = store.intersect_cpu(0, 1);
        let expected = bm_a & bm_b;
        assert_eq!(result, expected);
    }

    #[test]
    fn flat_bitmap_empty() {
        let mut store = FlatBitmapStore::new(256);
        let bm = Bitmap::new();
        store.add_bitmap(&bm);
        assert_eq!(store.get_bitmap(0), bm);
    }

    #[test]
    fn flat_bitmap_data_bytes() {
        let store = FlatBitmapStore::new(1024);
        assert_eq!(store.words_per_bitmap, 16); // 1024/64
        assert_eq!(store.data_bytes(), 0); // no bitmaps added yet
    }

    #[test]
    fn hybrid_store_build_and_lookup() {
        let mut terms = FxHashMap::default();

        // Dense term: many docs
        let mut dense_bm = Bitmap::new();
        dense_bm.add_range(0..1000);
        terms.insert("common".to_string(), dense_bm.clone());

        // Sparse term: few docs
        let mut sparse_bm = Bitmap::new();
        sparse_bm.add(5);
        sparse_bm.add(10);
        terms.insert("rare".to_string(), sparse_bm.clone());

        // Budget for 1 dense bitmap: 2000 docs → 32 words → 256 bytes
        let store = HybridBitmapStore::build(terms, 2000, 256);

        assert_eq!(store.dense_count(), 1);
        assert_eq!(store.sparse_count(), 1);
        assert_eq!(store.total_terms(), 2);

        // "common" should be dense
        match store.lookup("common") {
            Some(BitmapRef::Dense(id)) => {
                let readback = store.dense.get_bitmap(id);
                assert_eq!(readback, dense_bm);
            }
            _ => panic!("expected Dense for 'common'"),
        }

        // "rare" should be sparse
        match store.lookup("rare") {
            Some(BitmapRef::Sparse(bm)) => {
                assert_eq!(*bm, sparse_bm);
            }
            _ => panic!("expected Sparse for 'rare'"),
        }

        // Unknown term
        assert!(store.lookup("missing").is_none());
    }

    #[test]
    fn hybrid_store_vram_budget_limits_dense() {
        let mut terms = FxHashMap::default();

        for i in 0..10 {
            let mut bm = Bitmap::new();
            bm.add_range(0..(i + 1) * 100);
            terms.insert(format!("term_{i}"), bm);
        }

        // 1000 docs → 16 words per bitmap → 128 bytes per bitmap
        // Budget 384 bytes = room for 3 dense bitmaps
        let store = HybridBitmapStore::build(terms, 1000, 384);

        assert_eq!(store.dense_count(), 3);
        assert_eq!(store.sparse_count(), 7);
    }

    #[test]
    fn hybrid_store_empty() {
        let terms = FxHashMap::default();
        let store = HybridBitmapStore::build(terms, 1000, 1024 * 1024);
        assert_eq!(store.dense_count(), 0);
        assert_eq!(store.sparse_count(), 0);
    }

    #[test]
    fn flat_bitmap_roundtrip_sparse_bits() {
        let mut store = FlatBitmapStore::new(1000);
        let mut bm = Bitmap::new();
        bm.add(7);
        bm.add(100);
        bm.add(511);
        bm.add(999);
        store.add_bitmap(&bm);
        assert_eq!(store.get_bitmap(0), bm);
    }
}
