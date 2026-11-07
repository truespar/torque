use rustc_hash::FxHashMap;
use torque::bitmap::Bitmap;
use torque::gpu::bitmap::{BitmapRef, FlatBitmapStore, HybridBitmapStore};

// ============================================================
// Unit tests for FlatBitmapStore (always run, no GPU needed)
// ============================================================

#[test]
fn flat_bitmap_roundtrip() {
    let mut store = FlatBitmapStore::new(256);
    let mut bm = Bitmap::new();
    bm.add(0);
    bm.add(7);
    bm.add(63);
    bm.add(64);
    bm.add(127);
    bm.add(200);
    bm.add(255);

    let id = store.add_bitmap(&bm);
    assert_eq!(id, 0);
    assert_eq!(store.get_bitmap(0), bm);
}

#[test]
fn flat_bitmap_multiple_bitmaps() {
    let mut store = FlatBitmapStore::new(128);

    let mut bm1 = Bitmap::new();
    bm1.add_range(0..64);
    let mut bm2 = Bitmap::new();
    bm2.add_range(64..128);
    let mut bm3 = Bitmap::new();
    bm3.add_range(32..96);

    let id1 = store.add_bitmap(&bm1);
    let id2 = store.add_bitmap(&bm2);
    let id3 = store.add_bitmap(&bm3);

    assert_eq!(id1, 0);
    assert_eq!(id2, 1);
    assert_eq!(id3, 2);
    assert_eq!(store.num_bitmaps, 3);
    assert_eq!(store.get_bitmap(0), bm1);
    assert_eq!(store.get_bitmap(1), bm2);
    assert_eq!(store.get_bitmap(2), bm3);
}

#[test]
fn flat_bitmap_intersect_cpu_correct() {
    let mut store = FlatBitmapStore::new(256);

    let mut bm_a = Bitmap::new();
    bm_a.add_range(10..150);
    let mut bm_b = Bitmap::new();
    bm_b.add_range(100..200);

    store.add_bitmap(&bm_a);
    store.add_bitmap(&bm_b);

    let result = store.intersect_cpu(0, 1);
    let expected = &bm_a & &bm_b;
    assert_eq!(result, expected);
    assert_eq!(result.cardinality() as u64, 50); // 100..150
}

#[test]
fn flat_bitmap_empty_intersection() {
    let mut store = FlatBitmapStore::new(128);

    let mut bm_a = Bitmap::new();
    bm_a.add_range(0..32);
    let mut bm_b = Bitmap::new();
    bm_b.add_range(64..96);

    store.add_bitmap(&bm_a);
    store.add_bitmap(&bm_b);

    let result = store.intersect_cpu(0, 1);
    assert!(result.is_empty());
}

#[test]
fn flat_bitmap_words_per_bitmap() {
    // 100 docs → ceil(100/64) = 2 words
    let store = FlatBitmapStore::new(100);
    assert_eq!(store.words_per_bitmap, 2);

    // 64 docs → exactly 1 word
    let store = FlatBitmapStore::new(64);
    assert_eq!(store.words_per_bitmap, 1);

    // 65 docs → 2 words
    let store = FlatBitmapStore::new(65);
    assert_eq!(store.words_per_bitmap, 2);

    // 1 doc → 1 word
    let store = FlatBitmapStore::new(1);
    assert_eq!(store.words_per_bitmap, 1);
}

#[test]
fn flat_bitmap_data_layout() {
    let mut store = FlatBitmapStore::new(64);
    assert_eq!(store.words_per_bitmap, 1);

    let mut bm1 = Bitmap::new();
    bm1.add(0);
    let mut bm2 = Bitmap::new();
    bm2.add(63);

    store.add_bitmap(&bm1);
    store.add_bitmap(&bm2);

    assert_eq!(store.data.len(), 2);
    assert_eq!(store.data[0], 1u64); // bit 0
    assert_eq!(store.data[1], 1u64 << 63); // bit 63
    assert_eq!(store.data_bytes(), 16); // 2 * 8 bytes
}

// ============================================================
// Unit tests for HybridBitmapStore (always run, no GPU needed)
// ============================================================

#[test]
fn hybrid_store_dense_sparse_routing() {
    let mut terms = FxHashMap::default();

    // High-frequency term
    let mut dense_bm = Bitmap::new();
    dense_bm.add_range(0..500);
    terms.insert("popular".to_string(), dense_bm.clone());

    // Medium-frequency term
    let mut medium_bm = Bitmap::new();
    medium_bm.add_range(0..100);
    terms.insert("medium".to_string(), medium_bm.clone());

    // Low-frequency term
    let mut sparse_bm = Bitmap::new();
    sparse_bm.add(42);
    terms.insert("rare".to_string(), sparse_bm.clone());

    // Budget for 2 dense bitmaps: 1000 docs → 16 words → 128 bytes/bitmap → 256 bytes for 2
    let store = HybridBitmapStore::build(terms, 1000, 256);

    assert_eq!(store.dense_count(), 2);
    assert_eq!(store.sparse_count(), 1);

    // "popular" (highest freq) should be dense
    match store.lookup("popular") {
        Some(BitmapRef::Dense(id)) => {
            assert_eq!(store.dense.get_bitmap(id), dense_bm);
        }
        _ => panic!("expected Dense for 'popular'"),
    }

    // "medium" (second highest) should be dense
    match store.lookup("medium") {
        Some(BitmapRef::Dense(id)) => {
            assert_eq!(store.dense.get_bitmap(id), medium_bm);
        }
        _ => panic!("expected Dense for 'medium'"),
    }

    // "rare" should be sparse (budget full)
    match store.lookup("rare") {
        Some(BitmapRef::Sparse(bm)) => {
            assert_eq!(*bm, sparse_bm);
        }
        _ => panic!("expected Sparse for 'rare'"),
    }
}

#[test]
fn hybrid_store_missing_term() {
    let terms = FxHashMap::default();
    let store = HybridBitmapStore::build(terms, 1000, 1024 * 1024);
    assert!(store.lookup("nonexistent").is_none());
}

#[test]
fn hybrid_store_all_dense() {
    let mut terms = FxHashMap::default();
    for i in 0..5 {
        let mut bm = Bitmap::new();
        bm.add(i);
        terms.insert(format!("term_{i}"), bm);
    }

    // Very large budget → all terms go dense
    let store = HybridBitmapStore::build(terms, 100, 10 * 1024 * 1024);
    assert_eq!(store.dense_count(), 5);
    assert_eq!(store.sparse_count(), 0);
}

#[test]
fn hybrid_store_all_sparse() {
    let mut terms = FxHashMap::default();
    for i in 0..5 {
        let mut bm = Bitmap::new();
        bm.add(i);
        terms.insert(format!("term_{i}"), bm);
    }

    // Zero budget → all terms go sparse
    let store = HybridBitmapStore::build(terms, 100, 0);
    assert_eq!(store.dense_count(), 0);
    assert_eq!(store.sparse_count(), 5);
}

#[test]
fn hybrid_store_vram_budget_respected() {
    let mut terms = FxHashMap::default();
    for i in 0..100 {
        let mut bm = Bitmap::new();
        bm.add_range(0..(i + 1) * 10);
        terms.insert(format!("term_{i}"), bm);
    }

    // 10000 docs → 157 words/bitmap → 1256 bytes/bitmap
    // Budget 5000 bytes → fits ~3 dense bitmaps
    let store = HybridBitmapStore::build(terms, 10000, 5000);
    let expected_max = 5000 / (((10000 + 63) / 64) * 8);
    assert!(store.dense_count() <= expected_max + 1);
    assert!(store.dense_count() > 0);
}

// ============================================================
// GPU integration tests (only run when cuda feature is enabled)
// ============================================================

#[cfg(feature = "cuda")]
mod gpu_tests {
    use super::*;
    use std::sync::Arc;
    use torque::gpu::GpuContext;
    use torque::gpu::dispatch;
    use torque::gpu::kernels::KernelSet;
    use torque::gpu::memory::GpuMemoryManager;
    use torque::schema::VecDist;
    use torque::vector::quantize::RaBitQuantizer;
    use torque::vector::gpu::{self as vec_gpu, GpuSearchBuffers, VectorKernelSet};

    /// Try to get a GPU context + kernels. Returns None if no GPU is available,
    /// allowing tests to skip gracefully on machines without NVIDIA GPUs.
    fn try_gpu() -> Option<(Arc<GpuContext>, KernelSet)> {
        let gpu = GpuContext::try_init()?;
        let kernels = KernelSet::load(&gpu.module).ok()?;
        Some((Arc::new(gpu), kernels))
    }

    #[test]
    fn gpu_context_init() {
        let Some(gpu) = GpuContext::try_init() else {
            eprintln!("Skipping gpu_context_init: no GPU available");
            return;
        };
        assert!(!gpu.device_name.is_empty());
        assert!(gpu.sm_version >= 50);
        assert!(gpu.vram_bytes > 0);
    }

    #[test]
    fn gpu_bitmap_and() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let total_docs = 1024u32;

        let mut store = FlatBitmapStore::new(total_docs);
        let mut bm_a = Bitmap::new();
        bm_a.add_range(0..512);
        let mut bm_b = Bitmap::new();
        bm_b.add_range(256..768);

        store.add_bitmap(&bm_a);
        store.add_bitmap(&bm_b);

        let mut mem = GpuMemoryManager::new(gpu.ctx.clone(), gpu.stream.clone());
        mem.upload_bitmaps(&store).unwrap();

        let offset_a = store.bitmap_word_offset(0) as u32;
        let offset_b = store.bitmap_word_offset(1) as u32;
        let num_words = store.words_per_bitmap as u32;

        let gpu_result = dispatch::dispatch_bitmap_and(
            &gpu.stream, &kernels, &mut mem, offset_a, offset_b, num_words,
        ).unwrap();

        let cpu_result = &bm_a & &bm_b;
        assert_eq!(gpu_result, cpu_result);
        assert_eq!(gpu_result.cardinality() as u64, 256); // 256..512
    }

    #[test]
    fn gpu_multi_and() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let total_docs = 512u32;

        let mut store = FlatBitmapStore::new(total_docs);
        let bitmaps: Vec<Bitmap> = (0..5).map(|i| {
            let mut bm = Bitmap::new();
            bm.add_range(i * 50..300);
            bm
        }).collect();

        let offsets: Vec<u32> = bitmaps.iter().map(|bm| {
            let id = store.add_bitmap(bm);
            store.bitmap_word_offset(id) as u32
        }).collect();

        let mut mem = GpuMemoryManager::new(gpu.ctx.clone(), gpu.stream.clone());
        mem.upload_bitmaps(&store).unwrap();

        let num_words = store.words_per_bitmap as u32;
        let gpu_result = dispatch::dispatch_multi_and(
            &gpu.stream, &kernels, &mut mem, &offsets, num_words,
        ).unwrap();

        // CPU reference
        let mut cpu_result = bitmaps[0].clone();
        for bm in &bitmaps[1..] {
            cpu_result &= bm;
        }
        assert_eq!(gpu_result, cpu_result);
    }

    #[test]
    fn gpu_bitmap_or() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let total_docs = 256u32;

        let mut store = FlatBitmapStore::new(total_docs);
        let mut bm_a = Bitmap::new();
        bm_a.add_range(0..64);
        let mut bm_b = Bitmap::new();
        bm_b.add_range(192..256);

        store.add_bitmap(&bm_a);
        store.add_bitmap(&bm_b);

        let mut mem = GpuMemoryManager::new(gpu.ctx.clone(), gpu.stream.clone());
        mem.upload_bitmaps(&store).unwrap();

        let offset_a = store.bitmap_word_offset(0) as u32;
        let offset_b = store.bitmap_word_offset(1) as u32;
        let num_words = store.words_per_bitmap as u32;

        let gpu_result = dispatch::dispatch_bitmap_or(
            &gpu.stream, &kernels, &mut mem, offset_a, offset_b, num_words,
        ).unwrap();

        let cpu_result = &bm_a | &bm_b;
        assert_eq!(gpu_result, cpu_result);
        assert_eq!(gpu_result.cardinality() as u64, 128);
    }

    #[test]
    fn gpu_range_filter() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 1000u32;

        let values: Vec<i64> = (0..num_docs as i64).collect();
        let values_gpu = gpu.stream.clone_htod(&values).unwrap();

        let mut mem = GpuMemoryManager::new(gpu.ctx.clone(), gpu.stream.clone());

        let gpu_result = dispatch::dispatch_range_filter(
            &gpu.stream, &kernels, &mut mem, &values_gpu, num_docs, 100, 500,
        ).unwrap();

        // CPU reference
        let mut cpu_result = Bitmap::new();
        for (i, &v) in values.iter().enumerate() {
            if v >= 100 && v <= 500 {
                cpu_result.add(i as u32);
            }
        }
        assert_eq!(gpu_result, cpu_result);
        assert_eq!(gpu_result.cardinality() as u64, 401); // 100..=500
    }

    #[test]
    fn gpu_geo_radius() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 100u32;

        // Place docs at different latitudes along longitude 0
        let lats: Vec<f32> = (0..num_docs).map(|i| i as f32 * 0.1).collect();
        let lngs: Vec<f32> = vec![0.0f32; num_docs as usize];

        let mut mem = GpuMemoryManager::new(gpu.ctx.clone(), gpu.stream.clone());
        mem.upload_geo(&lats, &lngs).unwrap();

        // Search around (5.0, 0.0) with 50km radius
        let center_lat = 5.0f32;
        let center_lng = 0.0f32;
        let radius_km = 50.0f32;

        let gpu_result = dispatch::dispatch_geo_radius(
            &gpu.stream, &kernels, &mut mem, num_docs, center_lat, center_lng, radius_km,
        ).unwrap();

        // CPU reference using Haversine
        let mut cpu_result = Bitmap::new();
        for i in 0..num_docs {
            let dist = haversine_km(center_lat as f64, center_lng as f64, lats[i as usize] as f64, lngs[i as usize] as f64);
            if dist <= radius_km as f64 {
                cpu_result.add(i);
            }
        }

        // Allow slight differences due to f32 vs f64 precision
        let gpu_len = gpu_result.cardinality() as u64 as i64;
        let cpu_len = cpu_result.cardinality() as u64 as i64;
        assert!(
            (gpu_len - cpu_len).abs() <= 1,
            "GPU found {} docs, CPU found {} docs",
            gpu_len, cpu_len
        );
    }

    fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
        let r = 6371.0;
        let dlat = (lat2 - lat1).to_radians();
        let dlng = (lng2 - lng1).to_radians();
        let a = (dlat / 2.0).sin().powi(2)
            + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlng / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().asin();
        r * c
    }

    #[test]
    fn gpu_bm25f() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 500u32;
        let k1 = 1.2f32;
        let b = 0.75f32;
        let avg_dl = 100.0f32;
        let idf = 2.5f32;

        let term_freqs: Vec<u32> = (0..num_docs).map(|i| if i % 3 == 0 { 5 } else { 0 }).collect();
        let doc_lengths: Vec<u32> = (0..num_docs).map(|i| 50 + i).collect();

        let gpu_scores = dispatch::dispatch_bm25f(
            &gpu.stream, &kernels, &term_freqs, &doc_lengths, num_docs, avg_dl, idf, k1, b,
        ).unwrap();

        assert_eq!(gpu_scores.len(), num_docs as usize);

        // Verify against CPU BM25F
        for i in 0..num_docs as usize {
            let tf = term_freqs[i] as f32;
            let dl = doc_lengths[i] as f32;
            let expected = if tf > 0.0 {
                let norm = 1.0 - b + b * (dl / avg_dl);
                idf * (tf * (k1 + 1.0)) / (tf + k1 * norm)
            } else {
                0.0
            };

            let diff = (gpu_scores[i] - expected).abs();
            assert!(
                diff < 0.01,
                "Doc {}: GPU={}, CPU={}, diff={}",
                i, gpu_scores[i], expected, diff
            );
        }
    }

    #[test]
    fn gpu_facet_count() {
        let Some((gpu, kernels)) = try_gpu() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let total_docs = 256u32;
        let words_per_bitmap = (total_docs as usize + 63) / 64;

        // Create a result bitmap: docs 0..128
        let mut result_bm = Bitmap::new();
        result_bm.add_range(0..128);

        // Create 3 facet bitmaps
        let facet_bms = vec![
            {
                let mut bm = Bitmap::new();
                bm.add_range(0..64);
                bm
            },
            {
                let mut bm = Bitmap::new();
                bm.add_range(64..192);
                bm
            },
            {
                let mut bm = Bitmap::new();
                bm.add_range(100..256);
                bm
            },
        ];

        // Convert to flat u64 arrays
        let mut result_flat = vec![0u64; words_per_bitmap];
        for doc_id in result_bm.iter() {
            result_flat[doc_id as usize / 64] |= 1u64 << (doc_id as usize % 64);
        }

        let mut facets_flat = vec![0u64; words_per_bitmap * facet_bms.len()];
        for (f, bm) in facet_bms.iter().enumerate() {
            for doc_id in bm.iter() {
                let offset = f * words_per_bitmap + doc_id as usize / 64;
                facets_flat[offset] |= 1u64 << (doc_id as usize % 64);
            }
        }

        let result_gpu = gpu.stream.clone_htod(&result_flat).unwrap();
        let facets_gpu = gpu.stream.clone_htod(&facets_flat).unwrap();

        let gpu_counts = dispatch::dispatch_facet_count(
            &gpu.stream,
            &kernels,
            &result_gpu,
            &facets_gpu,
            words_per_bitmap as u32,
            facet_bms.len() as u32,
        ).unwrap();

        // CPU reference: intersection cardinality
        let expected: Vec<u32> = facet_bms
            .iter()
            .map(|f| (f & &result_bm).cardinality() as u32)
            .collect();

        assert_eq!(gpu_counts, expected);
        assert_eq!(expected[0], 64);  // 0..64 ∩ 0..128
        assert_eq!(expected[1], 64);  // 64..192 ∩ 0..128
        assert_eq!(expected[2], 28);  // 100..256 ∩ 0..128
    }

    // ============================================================
    // GPU vector search tests (Session 16)
    // ============================================================

    fn try_gpu_with_vec_kernels() -> Option<(Arc<GpuContext>, VectorKernelSet)> {
        let gpu = GpuContext::try_init()?;
        let vec_kernels = VectorKernelSet::load(&gpu.module).ok()?;
        Some((Arc::new(gpu), vec_kernels))
    }

    /// Generate deterministic test vectors: each doc's vector is [i*0.1, i*0.1+0.01, ...].
    fn generate_test_vectors(num_docs: u32, num_dim: u32) -> Vec<f32> {
        let mut vectors = vec![0.0f32; num_docs as usize * num_dim as usize];
        for doc in 0..num_docs as usize {
            for d in 0..num_dim as usize {
                vectors[doc * num_dim as usize + d] =
                    (doc as f32 * 0.1) + (d as f32 * 0.01);
            }
        }
        vectors
    }

    #[test]
    fn gpu_vec_cosine_f32_matches_cpu() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 200u32;
        let num_dim = 32u32;
        let k = 5;
        let metric = VecDist::Cosine;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 10.0 + d as f32 * 0.05).collect();

        let vectors_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        let gpu_results = vec_gpu::dispatch_vec_search_f32(
            &gpu.stream, &vec_kernels, &vectors_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        // CPU reference
        let mut cpu_distances: Vec<(u32, f32)> = (0..num_docs)
            .map(|i| {
                let offset = i as usize * num_dim as usize;
                let doc_vec = &vectors[offset..offset + num_dim as usize];
                (i, metric.compute(&query, doc_vec))
            })
            .collect();
        cpu_distances.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        cpu_distances.truncate(k);

        assert_eq!(gpu_results.len(), k);
        // Top-K doc IDs should match
        for i in 0..k {
            assert_eq!(
                gpu_results[i].0, cpu_distances[i].0,
                "Rank {i}: GPU doc {} != CPU doc {}",
                gpu_results[i].0, cpu_distances[i].0
            );
            let diff = (gpu_results[i].1 - cpu_distances[i].1).abs();
            assert!(diff < 1e-4, "Rank {i}: GPU dist {} != CPU dist {}, diff={diff}",
                gpu_results[i].1, cpu_distances[i].1);
        }
    }

    #[test]
    fn gpu_vec_l2_f32_matches_cpu() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 100u32;
        let num_dim = 16u32;
        let k = 3;
        let metric = VecDist::Euclidean;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 5.0 + d as f32 * 0.02).collect();

        let vectors_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        let gpu_results = vec_gpu::dispatch_vec_search_f32(
            &gpu.stream, &vec_kernels, &vectors_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        // CPU reference (lower is better for L2)
        let mut cpu_distances: Vec<(u32, f32)> = (0..num_docs)
            .map(|i| {
                let offset = i as usize * num_dim as usize;
                let doc_vec = &vectors[offset..offset + num_dim as usize];
                (i, metric.compute(&query, doc_vec))
            })
            .collect();
        cpu_distances.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        cpu_distances.truncate(k);

        assert_eq!(gpu_results.len(), k);
        for i in 0..k {
            assert_eq!(gpu_results[i].0, cpu_distances[i].0,
                "Rank {i}: GPU doc {} != CPU doc {}", gpu_results[i].0, cpu_distances[i].0);
        }
    }

    #[test]
    fn gpu_vec_f16_matches_f32() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 100u32;
        let num_dim = 64u32;
        let k = 5;
        let metric = VecDist::Cosine;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 5.0 + d as f32 * 0.03).collect();

        // f32 reference on GPU
        let vectors_f32_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();
        let f32_results = vec_gpu::dispatch_vec_search_f32(
            &gpu.stream, &vec_kernels, &vectors_f32_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        // f16 on GPU
        let vectors_f16 = vec_gpu::f32_to_f16(&vectors);
        let vectors_f16_gpu = gpu.stream.clone_htod(&vectors_f16).unwrap();
        let f16_results = vec_gpu::dispatch_vec_search_f16(
            &gpu.stream, &vec_kernels, &vectors_f16_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        assert_eq!(f16_results.len(), k);
        // Top-K should mostly agree (f16 precision may cause slight reordering)
        // At minimum, the top-1 should match
        assert_eq!(f16_results[0].0, f32_results[0].0,
            "Top-1 mismatch: f16={} f32={}", f16_results[0].0, f32_results[0].0);

        // Distances should be close (within f16 tolerance)
        for i in 0..k {
            let diff = (f16_results[i].1 - f32_results[i].1).abs();
            assert!(diff < 0.05, "Rank {i}: f16={} f32={} diff={diff}",
                f16_results[i].1, f32_results[i].1);
        }
    }

    #[test]
    fn gpu_vec_rabitq_recall() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 500u32;
        let num_dim = 64u16;
        let k = 10;
        let rerank_factor = 5;
        let metric = VecDist::Cosine;

        let vectors = generate_test_vectors(num_docs, num_dim as u32);
        let query: Vec<f32> = (0..num_dim).map(|d| 25.0 + d as f32 * 0.02).collect();

        // CPU exact top-K
        let mut cpu_distances: Vec<(u32, f32)> = (0..num_docs)
            .map(|i| {
                let offset = i as usize * num_dim as usize;
                let doc_vec = &vectors[offset..offset + num_dim as usize];
                (i, metric.compute(&query, doc_vec))
            })
            .collect();
        cpu_distances.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        cpu_distances.truncate(k);
        let cpu_top_ids: Vec<u32> = cpu_distances.iter().map(|&(id, _)| id).collect();

        // RaBitQ encode
        let quantizer = RaBitQuantizer::new(num_dim, 42);
        let (codes, norms) = quantizer.encode_batch(&vectors);
        let code_words = RaBitQuantizer::codes_to_u32_words(&codes, num_dim);

        let codes_gpu = gpu.stream.clone_htod(&code_words).unwrap();
        let norms_gpu = gpu.stream.clone_htod(&norms).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        let rabitq_results = vec_gpu::dispatch_vec_search_rabitq(
            &gpu.stream, &vec_kernels, &codes_gpu, &norms_gpu,
            &query, &quantizer, num_docs, k, rerank_factor,
            &vectors, &metric, &mut bufs,
        ).unwrap();

        assert_eq!(rabitq_results.len(), k);

        // Check recall: at least 70% of top-K should match exact search
        let rabitq_ids: Vec<u32> = rabitq_results.iter().map(|&(id, _)| id).collect();
        let overlap = cpu_top_ids.iter().filter(|id| rabitq_ids.contains(id)).count();
        let recall = overlap as f32 / k as f32;
        assert!(recall >= 0.7,
            "RaBitQ recall {recall:.2} < 0.7. CPU top: {cpu_top_ids:?}, RaBitQ: {rabitq_ids:?}");
    }

    #[test]
    fn gpu_vec_filtered_search() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 256u32;
        let num_dim = 32u32;
        let k = 5;
        let metric = VecDist::Cosine;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 12.0 + d as f32 * 0.01).collect();

        // Create filter: only even-numbered docs pass
        let words_per_bitmap = ((num_docs + 63) / 64) as usize;
        let mut filter_bitmap = vec![0u64; words_per_bitmap];
        for doc_id in 0..num_docs {
            if doc_id % 2 == 0 {
                filter_bitmap[doc_id as usize / 64] |= 1u64 << (doc_id as usize % 64);
            }
        }

        let vectors_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        let gpu_results = vec_gpu::dispatch_vec_search_filtered(
            &gpu.stream, &vec_kernels, &vectors_gpu, &query,
            &filter_bitmap, num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        // All returned docs should be even-numbered
        for &(doc_id, _) in &gpu_results {
            assert_eq!(doc_id % 2, 0, "Filtered doc {doc_id} is odd, should be even");
        }

        // CPU reference: only compute for even docs
        let mut cpu_distances: Vec<(u32, f32)> = (0..num_docs)
            .filter(|i| i % 2 == 0)
            .map(|i| {
                let offset = i as usize * num_dim as usize;
                let doc_vec = &vectors[offset..offset + num_dim as usize];
                (i, metric.compute(&query, doc_vec))
            })
            .collect();
        cpu_distances.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        cpu_distances.truncate(k);

        assert_eq!(gpu_results.len(), k);
        for i in 0..k {
            assert_eq!(gpu_results[i].0, cpu_distances[i].0,
                "Rank {i}: GPU doc {} != CPU doc {}", gpu_results[i].0, cpu_distances[i].0);
        }
    }

    // ============================================================
    // cuBLAS vector search tests
    // ============================================================

    #[test]
    fn cublas_f32_cosine_matches_custom_kernel() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 200u32;
        let num_dim = 64u32;
        let k = 10;
        let metric = VecDist::Cosine;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 10.0 + d as f32 * 0.05).collect();
        let doc_norms = vec_gpu::compute_doc_norms(&vectors, num_docs, num_dim);

        let vectors_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        // Custom kernel
        let kernel_results = vec_gpu::dispatch_vec_search_f32(
            &gpu.stream, &vec_kernels, &vectors_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        // cuBLAS
        let cublas_results = vec_gpu::dispatch_vec_search_f32_cublas(
            &gpu.stream, &gpu.blas, &vectors_gpu, &query,
            Some(&doc_norms), num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        assert_eq!(cublas_results.len(), k);
        // Top-K doc IDs should match between kernel and cuBLAS
        for i in 0..k {
            assert_eq!(
                cublas_results[i].0, kernel_results[i].0,
                "Rank {i}: cuBLAS doc {} != kernel doc {}",
                cublas_results[i].0, kernel_results[i].0
            );
            let diff = (cublas_results[i].1 - kernel_results[i].1).abs();
            assert!(diff < 1e-3,
                "Rank {i}: cuBLAS dist {} != kernel dist {}, diff={diff}",
                cublas_results[i].1, kernel_results[i].1);
        }
    }

    #[test]
    fn cublas_f32_dot_product_matches_custom_kernel() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 150u32;
        let num_dim = 32u32;
        let k = 5;
        let metric = VecDist::DotProduct;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 7.0 + d as f32 * 0.03).collect();

        let vectors_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        let kernel_results = vec_gpu::dispatch_vec_search_f32(
            &gpu.stream, &vec_kernels, &vectors_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        let cublas_results = vec_gpu::dispatch_vec_search_f32_cublas(
            &gpu.stream, &gpu.blas, &vectors_gpu, &query,
            None, num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        assert_eq!(cublas_results.len(), k);
        for i in 0..k {
            assert_eq!(cublas_results[i].0, kernel_results[i].0,
                "Rank {i}: cuBLAS doc {} != kernel doc {}",
                cublas_results[i].0, kernel_results[i].0);
        }
    }

    #[test]
    fn cublas_f32_l2_matches_custom_kernel() {
        let Some((gpu, vec_kernels)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 100u32;
        let num_dim = 16u32;
        let k = 5;
        let metric = VecDist::Euclidean;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 5.0 + d as f32 * 0.02).collect();
        let doc_norms = vec_gpu::compute_doc_norms(&vectors, num_docs, num_dim);

        let vectors_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();

        let kernel_results = vec_gpu::dispatch_vec_search_f32(
            &gpu.stream, &vec_kernels, &vectors_gpu, &query,
            num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        let cublas_results = vec_gpu::dispatch_vec_search_f32_cublas(
            &gpu.stream, &gpu.blas, &vectors_gpu, &query,
            Some(&doc_norms), num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        assert_eq!(cublas_results.len(), k);
        for i in 0..k {
            assert_eq!(cublas_results[i].0, kernel_results[i].0,
                "Rank {i}: cuBLAS doc {} != kernel doc {}",
                cublas_results[i].0, kernel_results[i].0);
            let diff = (cublas_results[i].1 - kernel_results[i].1).abs();
            assert!(diff < 0.1,
                "Rank {i}: cuBLAS L2 {} != kernel L2 {}, diff={diff}",
                cublas_results[i].1, kernel_results[i].1);
        }
    }

    #[test]
    fn cublas_f16_cosine_matches_f32() {
        let Some((gpu, _)) = try_gpu_with_vec_kernels() else {
            eprintln!("Skipping: no GPU available");
            return;
        };
        let num_docs = 100u32;
        let num_dim = 64u32;
        let k = 5;
        let metric = VecDist::Cosine;

        let vectors = generate_test_vectors(num_docs, num_dim);
        let query: Vec<f32> = (0..num_dim).map(|d| 5.0 + d as f32 * 0.03).collect();
        let doc_norms = vec_gpu::compute_doc_norms(&vectors, num_docs, num_dim);

        // cuBLAS f32
        let vectors_f32_gpu = gpu.stream.clone_htod(&vectors).unwrap();
        let mut bufs = GpuSearchBuffers::new(&gpu.stream, num_dim as usize, num_docs as usize).unwrap();
        let f32_results = vec_gpu::dispatch_vec_search_f32_cublas(
            &gpu.stream, &gpu.blas, &vectors_f32_gpu, &query,
            Some(&doc_norms), num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        // cuBLAS f16
        let vectors_f16 = vec_gpu::f32_to_f16(&vectors);
        let vectors_f16_gpu = gpu.stream.clone_htod(&vectors_f16).unwrap();
        let f16_results = vec_gpu::dispatch_vec_search_f16_cublas(
            &gpu.stream, &gpu.blas, &vectors_f16_gpu, &query,
            Some(&doc_norms), num_docs, num_dim, &metric, k, &mut bufs,
        ).unwrap();

        assert_eq!(f16_results.len(), k);
        // Top-1 should match
        assert_eq!(f16_results[0].0, f32_results[0].0,
            "cuBLAS f16 top-1 {} != f32 top-1 {}", f16_results[0].0, f32_results[0].0);

        // Distances should be close
        for i in 0..k {
            let diff = (f16_results[i].1 - f32_results[i].1).abs();
            assert!(diff < 0.05,
                "Rank {i}: cuBLAS f16={} f32={} diff={diff}",
                f16_results[i].1, f32_results[i].1);
        }
    }
}
