use crate::schema::VecDist;
use std::sync::OnceLock;

/// Cosine similarity: dot(a,b) / (|a| * |b|). Returns -1.0 to 1.0.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Dot product: sum(a[i] * b[i]).
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Euclidean (L2) distance: sqrt(sum((a[i] - b[i])^2)).
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    euclidean_distance_squared(a, b).sqrt()
}

/// Squared Euclidean (L2²) distance: sum((a[i] - b[i])^2).
/// Preserves ordering without the sqrt, which is monotonic.
pub fn euclidean_distance_squared(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
}

/// Normalize a single vector in-place to unit length. Zero vectors left as-is.
pub fn normalize_vector(v: &mut [f32]) {
    let norm_sq: f32 = v.iter().map(|x| x * x).sum();
    if norm_sq > 0.0 {
        let inv_norm = 1.0 / norm_sq.sqrt();
        for x in v.iter_mut() {
            *x *= inv_norm;
        }
    }
}

/// Normalize all vectors in a flat array to unit length.
/// `vectors` layout: [doc0_d0, doc0_d1, ..., doc1_d0, ...].
pub fn normalize_vectors(vectors: &mut [f32], num_docs: usize, dim: usize) {
    for i in 0..num_docs {
        let start = i * dim;
        normalize_vector(&mut vectors[start..start + dim]);
    }
}

// --- SIMD distance kernels (x86_64 AVX2+FMA) ---

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_op_in_unsafe_fn)]
mod simd {
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn dot_product_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let n = a.len();
        let chunks = n / 8;
        let mut acc = _mm256_setzero_ps();

        for i in 0..chunks {
            let va = _mm256_loadu_ps(a.as_ptr().add(i * 8));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i * 8));
            acc = _mm256_fmadd_ps(va, vb, acc);
        }

        // Horizontal sum of 8 floats
        let hi = _mm256_extractf128_ps(acc, 1);
        let lo = _mm256_castps256_ps128(acc);
        let sum4 = _mm_add_ps(lo, hi);
        let shuf = _mm_movehdup_ps(sum4);
        let sums = _mm_add_ps(sum4, shuf);
        let shuf2 = _mm_movehl_ps(sums, sums);
        let sums2 = _mm_add_ss(sums, shuf2);
        let mut result = _mm_cvtss_f32(sums2);

        // Scalar tail
        for i in (chunks * 8)..n {
            result += a[i] * b[i];
        }

        result
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn l2_squared_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let n = a.len();
        let chunks = n / 8;
        let mut acc = _mm256_setzero_ps();

        for i in 0..chunks {
            let va = _mm256_loadu_ps(a.as_ptr().add(i * 8));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i * 8));
            let diff = _mm256_sub_ps(va, vb);
            acc = _mm256_fmadd_ps(diff, diff, acc);
        }

        // Horizontal sum
        let hi = _mm256_extractf128_ps(acc, 1);
        let lo = _mm256_castps256_ps128(acc);
        let sum4 = _mm_add_ps(lo, hi);
        let shuf = _mm_movehdup_ps(sum4);
        let sums = _mm_add_ps(sum4, shuf);
        let shuf2 = _mm_movehl_ps(sums, sums);
        let sums2 = _mm_add_ss(sums, shuf2);
        let mut result = _mm_cvtss_f32(sums2);

        // Scalar tail
        for i in (chunks * 8)..n {
            let d = a[i] - b[i];
            result += d * d;
        }

        result
    }
}

/// Cached AVX2+FMA feature detection.
#[cfg(target_arch = "x86_64")]
fn has_avx2_fma() -> bool {
    static DETECTED: OnceLock<bool> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
    })
}

impl VecDist {
    /// Compute distance/similarity between two vectors.
    pub fn compute(&self, a: &[f32], b: &[f32]) -> f32 {
        match self {
            VecDist::Cosine => cosine_similarity(a, b),
            VecDist::DotProduct => dot_product(a, b),
            VecDist::Euclidean => euclidean_distance(a, b),
        }
    }

    /// Fast distance for HNSW ordering — squared L2, SIMD when available.
    /// Only preserves ordering, not actual distance values.
    pub fn compute_fast(&self, a: &[f32], b: &[f32]) -> f32 {
        #[cfg(target_arch = "x86_64")]
        if has_avx2_fma() {
            return match self {
                VecDist::DotProduct => {
                    // SAFETY: has_avx2_fma() confirmed the feature is available
                    unsafe { simd::dot_product_avx2_fma(a, b) }
                }
                VecDist::Euclidean => {
                    unsafe { simd::l2_squared_avx2_fma(a, b) }
                }
                // Cosine needs normalization — no SIMD shortcut.
                // VectorIndex pre-normalizes and routes as DotProduct.
                VecDist::Cosine => cosine_similarity(a, b),
            };
        }

        // Scalar fallback
        match self {
            VecDist::Cosine => cosine_similarity(a, b),
            VecDist::DotProduct => dot_product(a, b),
            VecDist::Euclidean => euclidean_distance_squared(a, b),
        }
    }

    /// Whether higher values mean more similar (for sorting).
    /// Cosine and dot product: higher = more similar.
    /// Euclidean: lower = more similar.
    pub fn higher_is_better(&self) -> bool {
        match self {
            VecDist::Cosine | VecDist::DotProduct => true,
            VecDist::Euclidean => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_vector() {
        let a = vec![1.0, 2.0];
        let b = vec![0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn dot_product_known() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        // 1*4 + 2*5 + 3*6 = 4 + 10 + 18 = 32
        assert!((dot_product(&a, &b) - 32.0).abs() < 1e-6);
    }

    #[test]
    fn euclidean_known() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        // sqrt(9+16) = 5
        assert!((euclidean_distance(&a, &b) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn euclidean_same_point() {
        let v = vec![1.0, 2.0, 3.0];
        assert!(euclidean_distance(&v, &v) < 1e-6);
    }

    #[test]
    fn metric_higher_is_better() {
        assert!(VecDist::Cosine.higher_is_better());
        assert!(VecDist::DotProduct.higher_is_better());
        assert!(!VecDist::Euclidean.higher_is_better());
    }

    // --- Optimization 1 tests ---

    #[test]
    fn euclidean_squared_known() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        // 9 + 16 = 25 (no sqrt)
        assert!((euclidean_distance_squared(&a, &b) - 25.0).abs() < 1e-6);
    }

    #[test]
    fn compute_fast_preserves_ordering() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![5.0, 6.0, 7.0, 8.0];
        let c = vec![10.0, 20.0, 30.0, 40.0];

        // Euclidean: ordering should be preserved between compute and compute_fast
        let dist_ab = VecDist::Euclidean.compute(&a, &b);
        let dist_ac = VecDist::Euclidean.compute(&a, &c);
        let fast_ab = VecDist::Euclidean.compute_fast(&a, &b);
        let fast_ac = VecDist::Euclidean.compute_fast(&a, &c);

        assert_eq!(
            dist_ab < dist_ac,
            fast_ab < fast_ac,
            "Squared L2 should preserve ordering"
        );

        // DotProduct: compute_fast should give same value
        let dp_compute = VecDist::DotProduct.compute(&a, &b);
        let dp_fast = VecDist::DotProduct.compute_fast(&a, &b);
        assert!((dp_compute - dp_fast).abs() < 1e-4, "DotProduct fast should match scalar");
    }

    // --- Optimization 2 tests ---

    #[test]
    fn normalized_cosine_equals_dot() {
        let mut a = vec![3.0, 4.0, 0.0];
        let mut b = vec![1.0, 2.0, 2.0];
        let cosine_val = cosine_similarity(&a, &b);

        normalize_vector(&mut a);
        normalize_vector(&mut b);
        let dot_val = dot_product(&a, &b);

        assert!(
            (cosine_val - dot_val).abs() < 1e-5,
            "Cosine of raw vectors ({cosine_val}) should equal dot of normalized ({dot_val})"
        );
    }

    #[test]
    fn normalize_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        normalize_vector(&mut v);
        // Zero vector should remain zero
        assert!(v.iter().all(|&x| x == 0.0));
    }

    // --- Optimization 3 tests ---

    #[test]
    fn simd_dot_matches_scalar() {
        let a: Vec<f32> = (0..128).map(|i| (i as f32) * 0.01).collect();
        let b: Vec<f32> = (0..128).map(|i| (i as f32) * 0.02 - 1.0).collect();

        let scalar = dot_product(&a, &b);
        let fast = VecDist::DotProduct.compute_fast(&a, &b);

        assert!(
            (scalar - fast).abs() < 1e-3,
            "SIMD dot ({fast}) should match scalar ({scalar})"
        );
    }

    #[test]
    fn simd_l2sq_matches_scalar() {
        let a: Vec<f32> = (0..128).map(|i| (i as f32) * 0.01).collect();
        let b: Vec<f32> = (0..128).map(|i| (i as f32) * 0.02 - 1.0).collect();

        let scalar = euclidean_distance_squared(&a, &b);
        let fast = VecDist::Euclidean.compute_fast(&a, &b);

        assert!(
            (scalar - fast).abs() < 1e-3,
            "SIMD L2sq ({fast}) should match scalar ({scalar})"
        );
    }

    #[test]
    fn simd_odd_dimensions() {
        // 13 dims — not a multiple of 8, tests scalar tail handling
        let a: Vec<f32> = (0..13).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..13).map(|i| (i as f32) * 2.0).collect();

        let scalar_dot = dot_product(&a, &b);
        let fast_dot = VecDist::DotProduct.compute_fast(&a, &b);
        assert!((scalar_dot - fast_dot).abs() < 1e-3, "Odd-dim dot mismatch");

        let scalar_l2 = euclidean_distance_squared(&a, &b);
        let fast_l2 = VecDist::Euclidean.compute_fast(&a, &b);
        assert!((scalar_l2 - fast_l2).abs() < 1e-3, "Odd-dim L2sq mismatch");
    }
}
