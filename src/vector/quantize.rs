use crate::tqbf::bytes::Pods;

/// RaBitQ binary quantizer (SIGMOD 2024/2025).
/// Quantizes D-dimensional f32 vectors to D bits + scalar norm correction.
/// Achieves ~32x compression vs f32 with competitive recall.

/// RaBitQ quantizer: random orthogonal rotation + binarization.
#[derive(Clone)]
pub struct RaBitQuantizer {
    /// Random orthogonal rotation matrix, row-major [num_dim x num_dim].
    pub(crate) rotation: Pods<f32>,
    pub num_dim: u16,
}

impl RaBitQuantizer {
    /// Create a new quantizer with a random orthogonal rotation matrix.
    /// Uses deterministic pseudo-random generation from seed for reproducibility.
    pub fn new(num_dim: u16, seed: u64) -> Self {
        let rotation = generate_orthogonal_matrix(num_dim as usize, seed);
        Self { rotation: Pods::from_vec(rotation), num_dim }
    }

    /// A quantizer over a rotation read from a VECTORS section.
    pub(crate) fn from_rotation(rotation: Pods<f32>, num_dim: u16) -> Self {
        Self { rotation, num_dim }
    }

    /// Encode a single vector to a binary code + norm scalar.
    pub fn encode(&self, vector: &[f32]) -> (Vec<u8>, f32) {
        let rotated = self.rotate(vector);
        let code = binarize(&rotated);
        let norm = vector_norm(vector);
        (code, norm)
    }

    /// Encode a batch of vectors in parallel using rayon.
    /// Returns (all_codes concatenated, all_norms).
    pub fn encode_batch(&self, vectors: &[f32]) -> (Vec<u8>, Vec<f32>) {
        use rayon::prelude::*;

        let dim = self.num_dim as usize;
        let num_vecs = vectors.len() / dim;
        let bytes_per_code = (dim + 7) / 8;

        let mut codes = vec![0u8; num_vecs * bytes_per_code];
        let mut norms = vec![0f32; num_vecs];

        codes
            .par_chunks_mut(bytes_per_code)
            .zip(norms.par_iter_mut())
            .enumerate()
            .for_each(|(i, (code_slice, norm))| {
                let vec_start = i * dim;
                let vec_end = vec_start + dim;
                let (c, n) = self.encode(&vectors[vec_start..vec_end]);
                code_slice.copy_from_slice(&c);
                *norm = n;
            });

        (codes, norms)
    }

    /// Convert binary codes from byte representation to u32 words for GPU upload.
    /// GPU uses u32 words with __popc() for popcount.
    pub fn codes_to_u32_words(codes: &[u8], num_dim: u16) -> Vec<u32> {
        let bytes_per_code = (num_dim as usize + 7) / 8;
        let words_per_code = (num_dim as usize + 31) / 32;
        let num_codes = codes.len() / bytes_per_code;

        let mut words = vec![0u32; num_codes * words_per_code];
        for c in 0..num_codes {
            let code_start = c * bytes_per_code;
            let word_start = c * words_per_code;
            for b in 0..bytes_per_code {
                let byte_val = codes[code_start + b];
                let word_idx = b / 4;
                let byte_pos = b % 4;
                words[word_start + word_idx] |= (byte_val as u32) << (byte_pos * 8);
            }
        }
        words
    }

    /// Prepare a query vector for RaBitQ distance computation.
    /// Returns (binary_code, norm).
    pub fn prepare_query(&self, query: &[f32]) -> (Vec<u8>, f32) {
        self.encode(query)
    }

    /// CPU-side distance estimate using XOR + popcount.
    /// Returns an approximate distance proportional to hamming distance * norms.
    pub fn estimate_distance(
        query_code: &[u8],
        doc_code: &[u8],
        query_norm: f32,
        doc_norm: f32,
    ) -> f32 {
        let hamming: u32 = query_code
            .iter()
            .zip(doc_code)
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();
        hamming as f32 * query_norm * doc_norm
    }

    /// Number of bytes per binary code.
    pub fn bytes_per_code(&self) -> usize {
        (self.num_dim as usize + 7) / 8
    }

    /// Number of u32 words per binary code (for GPU).
    pub fn words_per_code(&self) -> usize {
        (self.num_dim as usize + 31) / 32
    }

    fn rotate(&self, vector: &[f32]) -> Vec<f32> {
        let d = self.num_dim as usize;
        let mut result = vec![0f32; d];
        for i in 0..d {
            let row_offset = i * d;
            let mut sum = 0f32;
            for j in 0..d {
                sum += self.rotation[row_offset + j] * vector[j];
            }
            result[i] = sum;
        }
        result
    }
}

/// Binarize a rotated vector: bit[i] = 1 if rotated[i] >= 0.
fn binarize(rotated: &[f32]) -> Vec<u8> {
    let num_bytes = (rotated.len() + 7) / 8;
    let mut code = vec![0u8; num_bytes];
    for (i, &val) in rotated.iter().enumerate() {
        if val >= 0.0 {
            code[i / 8] |= 1 << (i % 8);
        }
    }
    code
}

/// Compute the L2 norm of a vector.
fn vector_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Generate a random orthogonal matrix via Gram-Schmidt process.
/// Uses a deterministic PRNG seeded with `seed` for reproducibility.
fn generate_orthogonal_matrix(dim: usize, seed: u64) -> Vec<f32> {
    let mut rng = SimpleRng::new(seed);
    let mut matrix = vec![0f32; dim * dim];

    // Generate random vectors and orthogonalize via modified Gram-Schmidt
    for i in 0..dim {
        // Generate random vector
        for j in 0..dim {
            matrix[i * dim + j] = rng.next_gaussian();
        }

        // Orthogonalize against previous vectors
        for k in 0..i {
            let dot = dot_product_slice(
                &matrix[i * dim..(i + 1) * dim],
                &matrix[k * dim..(k + 1) * dim],
            );
            for j in 0..dim {
                matrix[i * dim + j] -= dot * matrix[k * dim + j];
            }
        }

        // Normalize
        let norm = vector_norm(&matrix[i * dim..(i + 1) * dim]);
        if norm > 1e-10 {
            for j in 0..dim {
                matrix[i * dim + j] /= norm;
            }
        }
    }

    matrix
}

fn dot_product_slice(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Simple deterministic PRNG (xoshiro256** variant) for reproducible matrix generation.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9E3779B97F4A7C15),
        }
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Generate approximate standard normal via Box-Muller transform.
    fn next_gaussian(&mut self) -> f32 {
        let u1 = self.next_f32().max(1e-10);
        let u2 = self.next_f32();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
    }
}

/// Auto-precision selection for GPU vector storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorPrecision {
    F32,
    F16,
    RaBitQ,
}

/// Configuration for GPU vector storage, selected based on VRAM budget.
#[derive(Debug, Clone)]
pub struct VectorGpuConfig {
    pub precision: VectorPrecision,
    pub gpu_buffer_bytes: u64,
}

impl VectorGpuConfig {
    /// Automatically select the best precision given available VRAM.
    /// Uses 75% of available VRAM as budget.
    pub fn auto(num_docs: u32, num_dim: u16, vram_available: u64) -> Self {
        let f32_bytes = num_docs as u64 * num_dim as u64 * 4;
        let f16_bytes = f32_bytes / 2;
        let rabitq_bytes = num_docs as u64 * ((num_dim as u64 + 7) / 8 + 4); // codes + norms

        let budget = (vram_available as f64 * 0.75) as u64;

        if f32_bytes <= budget {
            Self {
                precision: VectorPrecision::F32,
                gpu_buffer_bytes: f32_bytes,
            }
        } else if f16_bytes <= budget {
            Self {
                precision: VectorPrecision::F16,
                gpu_buffer_bytes: f16_bytes,
            }
        } else {
            Self {
                precision: VectorPrecision::RaBitQ,
                gpu_buffer_bytes: rabitq_bytes,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_roundtrip_code_length() {
        let q = RaBitQuantizer::new(128, 42);
        let vec = vec![0.1f32; 128];
        let (code, norm) = q.encode(&vec);
        assert_eq!(code.len(), 16); // 128 / 8
        assert!(norm > 0.0);
    }

    #[test]
    fn encode_32dim() {
        let q = RaBitQuantizer::new(32, 42);
        let vec = vec![1.0f32; 32];
        let (code, norm) = q.encode(&vec);
        assert_eq!(code.len(), 4); // 32 / 8
        assert!((norm - (32.0f32).sqrt()).abs() < 0.01);
    }

    #[test]
    fn encode_odd_dims() {
        let q = RaBitQuantizer::new(100, 42);
        let vec = vec![0.5f32; 100];
        let (code, _) = q.encode(&vec);
        assert_eq!(code.len(), 13); // ceil(100/8)
    }

    #[test]
    fn batch_encoding_matches_individual() {
        let dim = 64u16;
        let q = RaBitQuantizer::new(dim, 42);
        let mut vectors = vec![0f32; 10 * dim as usize];
        let mut rng = SimpleRng::new(99);
        for v in &mut vectors {
            *v = rng.next_f32() - 0.5;
        }

        let (batch_codes, batch_norms) = q.encode_batch(&vectors);
        let bpc = q.bytes_per_code();

        for i in 0..10 {
            let start = i * dim as usize;
            let (ind_code, ind_norm) = q.encode(&vectors[start..start + dim as usize]);
            assert_eq!(
                &batch_codes[i * bpc..(i + 1) * bpc],
                &ind_code[..],
                "Code mismatch for vector {i}"
            );
            assert!(
                (batch_norms[i] - ind_norm).abs() < 1e-6,
                "Norm mismatch for vector {i}"
            );
        }
    }

    #[test]
    fn reproducible_quantizer() {
        let q1 = RaBitQuantizer::new(64, 42);
        let q2 = RaBitQuantizer::new(64, 42);
        assert_eq!(q1.rotation[..], q2.rotation[..]);
    }

    #[test]
    fn different_seed_different_rotation() {
        let q1 = RaBitQuantizer::new(64, 42);
        let q2 = RaBitQuantizer::new(64, 43);
        assert_ne!(q1.rotation[..], q2.rotation[..]);
    }

    #[test]
    fn orthogonal_matrix_is_orthogonal() {
        let dim = 16;
        let mat = generate_orthogonal_matrix(dim, 42);

        // Check Q^T * Q ≈ I
        for i in 0..dim {
            for j in 0..dim {
                let dot: f32 = (0..dim)
                    .map(|k| mat[i * dim + k] * mat[j * dim + k])
                    .sum();
                if i == j {
                    assert!((dot - 1.0).abs() < 1e-4, "Diagonal [{i},{j}] = {dot}");
                } else {
                    assert!(dot.abs() < 1e-4, "Off-diagonal [{i},{j}] = {dot}");
                }
            }
        }
    }

    #[test]
    fn distance_estimate_identical_is_zero() {
        let q = RaBitQuantizer::new(128, 42);
        let vec = vec![1.0f32; 128];
        let (code, norm) = q.encode(&vec);
        let dist = RaBitQuantizer::estimate_distance(&code, &code, norm, norm);
        assert_eq!(dist, 0.0);
    }

    #[test]
    fn distance_estimate_different() {
        let q = RaBitQuantizer::new(64, 42);
        let v1 = vec![1.0f32; 64];
        let v2 = vec![-1.0f32; 64];
        let (c1, n1) = q.encode(&v1);
        let (c2, n2) = q.encode(&v2);
        let dist = RaBitQuantizer::estimate_distance(&c1, &c2, n1, n2);
        assert!(dist > 0.0, "Different vectors should have non-zero distance");
    }

    #[test]
    fn codes_to_u32_words_roundtrip() {
        let q = RaBitQuantizer::new(64, 42);
        let vec = vec![0.5f32; 64];
        let (code, _) = q.encode(&vec);
        let words = RaBitQuantizer::codes_to_u32_words(&code, 64);
        assert_eq!(words.len(), 2); // 64/32 = 2 words
    }

    #[test]
    fn auto_precision_f32_fits() {
        // 1M docs x 384 dims x 4 bytes = 1.5 GB, budget = 36 GB * 0.75 = 27 GB
        let config = VectorGpuConfig::auto(1_000_000, 384, 36 * 1024 * 1024 * 1024);
        assert_eq!(config.precision, VectorPrecision::F32);
    }

    #[test]
    fn auto_precision_f16_needed() {
        // 30M docs x 768 dims x 4 = 92 GB, but 48 GB VRAM → f16 = 46 GB fits budget 36 GB? No.
        // 30M x 768 x 2 = 46 GB > 36 GB budget. So RaBitQ.
        // Let's use 10M x 768: f32 = 30.7 GB > 36 GB budget? 10M*768*4 = 30.72 GB, budget = 36 GB, fits!
        // 20M x 768: f32 = 61.4 GB > 36 GB, f16 = 30.7 GB < 36 GB → f16
        let config = VectorGpuConfig::auto(20_000_000, 768, 48 * 1024 * 1024 * 1024);
        assert_eq!(config.precision, VectorPrecision::F16);
    }

    #[test]
    fn auto_precision_rabitq_needed() {
        // 30M x 1536 dims: f32 = 184 GB, f16 = 92 GB, both > 48 GB * 0.75 = 36 GB
        let config = VectorGpuConfig::auto(30_000_000, 1536, 48 * 1024 * 1024 * 1024);
        assert_eq!(config.precision, VectorPrecision::RaBitQ);
    }
}
