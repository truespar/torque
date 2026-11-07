use super::quantize::RaBitQuantizer;
use crate::gpu::kernels::launch_config;
use crate::schema::{DocId, VecDist};
use cudarc::cublas::{CudaBlas, Gemv, GemvConfig};
use cudarc::driver::{CudaFunction, CudaModule, CudaSlice, CudaStream, PushKernelArg};
use std::sync::Arc;

/// Kernel function handles for vector distance computation.
pub struct VectorKernelSet {
    pub cosine_f32: CudaFunction,
    pub dot_f32: CudaFunction,
    pub l2_f32: CudaFunction,
    pub cosine_f16: CudaFunction,
    pub dot_f16: CudaFunction,
    pub rabitq_distance: CudaFunction,
    pub cosine_filtered: CudaFunction,
    pub dot_filtered: CudaFunction,
    pub l2_filtered: CudaFunction,
    pub topk_threshold: CudaFunction,
}

impl VectorKernelSet {
    pub fn load(module: &Arc<CudaModule>) -> Result<Self, cudarc::driver::DriverError> {
        Ok(Self {
            cosine_f32: module.load_function("vec_cosine_f32")?,
            dot_f32: module.load_function("vec_dot_f32")?,
            l2_f32: module.load_function("vec_l2_f32")?,
            cosine_f16: module.load_function("vec_cosine_f16")?,
            dot_f16: module.load_function("vec_dot_f16")?,
            rabitq_distance: module.load_function("vec_rabitq_distance")?,
            cosine_filtered: module.load_function("vec_cosine_filtered")?,
            dot_filtered: module.load_function("vec_dot_filtered")?,
            l2_filtered: module.load_function("vec_l2_filtered")?,
            topk_threshold: module.load_function("vec_topk_threshold")?,
        })
    }

    fn select_f32_kernel(&self, metric: &VecDist) -> &CudaFunction {
        match metric {
            VecDist::Cosine => &self.cosine_f32,
            VecDist::DotProduct => &self.dot_f32,
            VecDist::Euclidean => &self.l2_f32,
        }
    }

    fn select_filtered_kernel(&self, metric: &VecDist) -> &CudaFunction {
        match metric {
            VecDist::Cosine => &self.cosine_filtered,
            VecDist::DotProduct => &self.dot_filtered,
            VecDist::Euclidean => &self.l2_filtered,
        }
    }

    fn select_f16_kernel(&self, metric: &VecDist) -> &CudaFunction {
        match metric {
            VecDist::Cosine => &self.cosine_f16,
            VecDist::DotProduct => &self.dot_f16,
            VecDist::Euclidean => &self.cosine_f16, // L2 f16 not implemented; fallback to cosine
        }
    }
}

/// Pre-allocated reusable GPU buffers for vector search dispatch.
/// Avoids cudaMalloc/cudaFree per query (~20-70µs overhead).
pub struct GpuSearchBuffers {
    pub query_f32: CudaSlice<f32>,
    pub query_f16: CudaSlice<half::f16>,
    pub query_u32: CudaSlice<u32>,
    pub distances_f32: CudaSlice<f32>,
    pub dots_f16: CudaSlice<half::f16>,
    pub filter_bitmap_gpu: CudaSlice<u64>,
    pub distances_host: Vec<f32>,
    pub filter_bitmap_host: Vec<u64>,
    pub topk_scratch: Vec<(DocId, f32)>,
    pub max_query_dim: usize,
    pub max_docs: usize,
    pub max_filter_words: usize,
}

impl GpuSearchBuffers {
    pub fn new(
        stream: &Arc<CudaStream>,
        initial_dim: usize,
        initial_docs: usize,
    ) -> Result<Self, cudarc::driver::DriverError> {
        let filter_words = (initial_docs + 63) / 64;
        let rabitq_words = initial_dim / 32;
        Ok(Self {
            query_f32: stream.alloc_zeros::<f32>(initial_dim)?,
            query_f16: stream.alloc_zeros::<half::f16>(initial_dim)?,
            query_u32: stream.alloc_zeros::<u32>(rabitq_words.max(1))?,
            distances_f32: stream.alloc_zeros::<f32>(initial_docs)?,
            dots_f16: stream.alloc_zeros::<half::f16>(initial_docs)?,
            filter_bitmap_gpu: stream.alloc_zeros::<u64>(filter_words)?,
            distances_host: vec![0.0f32; initial_docs],
            filter_bitmap_host: vec![0u64; filter_words],
            topk_scratch: Vec::with_capacity(initial_docs),
            max_query_dim: initial_dim,
            max_docs: initial_docs,
            max_filter_words: filter_words,
        })
    }

    pub fn ensure_capacity(
        &mut self,
        stream: &Arc<CudaStream>,
        dim: usize,
        docs: usize,
    ) -> Result<(), cudarc::driver::DriverError> {
        if dim > self.max_query_dim {
            let new_dim = dim.next_power_of_two();
            self.query_f32 = stream.alloc_zeros::<f32>(new_dim)?;
            self.query_f16 = stream.alloc_zeros::<half::f16>(new_dim)?;
            let rabitq_words = new_dim / 32;
            self.query_u32 = stream.alloc_zeros::<u32>(rabitq_words.max(1))?;
            self.max_query_dim = new_dim;
        }

        if docs > self.max_docs {
            let new_docs = docs.next_power_of_two();
            self.distances_f32 = stream.alloc_zeros::<f32>(new_docs)?;
            self.dots_f16 = stream.alloc_zeros::<half::f16>(new_docs)?;
            self.distances_host.resize(new_docs, 0.0);
            self.topk_scratch = Vec::with_capacity(new_docs);
            self.max_docs = new_docs;
        }

        let filter_words = (docs + 63) / 64;
        if filter_words > self.max_filter_words {
            let new_words = filter_words.next_power_of_two();
            self.filter_bitmap_gpu = stream.alloc_zeros::<u64>(new_words)?;
            self.filter_bitmap_host.resize(new_words, 0);
            self.max_filter_words = new_words;
        }

        Ok(())
    }
}

/// Dispatch f32 vector distance computation on GPU, return top-K results.
pub fn dispatch_vec_search_f32(
    stream: &Arc<CudaStream>,
    kernels: &VectorKernelSet,
    vectors_gpu: &CudaSlice<f32>,
    query: &[f32],
    num_docs: u32,
    num_dim: u32,
    metric: &VecDist,
    k: usize,
    bufs: &mut GpuSearchBuffers,
) -> Result<Vec<(DocId, f32)>, Box<dyn std::error::Error>> {
    bufs.ensure_capacity(stream, num_dim as usize, num_docs as usize)?;

    stream.memcpy_htod(query, &mut bufs.query_f32)?;
    stream.memset_zeros(&mut bufs.distances_f32)?;

    let func = kernels.select_f32_kernel(metric);
    let cfg = launch_config(num_docs);
    let mut builder = stream.launch_builder(func);
    builder.arg(vectors_gpu);
    builder.arg(&bufs.query_f32);
    builder.arg(&mut bufs.distances_f32);
    builder.arg(&num_docs);
    builder.arg(&num_dim);
    unsafe { builder.launch(cfg) }?;

    stream.synchronize()?;
    let n = num_docs as usize;
    stream.memcpy_dtoh(&bufs.distances_f32.slice(..n), &mut bufs.distances_host[..n])?;

    Ok(cpu_topk(&bufs.distances_host[..n], k, metric.higher_is_better()))
}

/// Dispatch f16 vector distance computation on GPU.
pub fn dispatch_vec_search_f16(
    stream: &Arc<CudaStream>,
    kernels: &VectorKernelSet,
    vectors_gpu: &CudaSlice<half::f16>,
    query: &[f32],
    num_docs: u32,
    num_dim: u32,
    metric: &VecDist,
    k: usize,
    bufs: &mut GpuSearchBuffers,
) -> Result<Vec<(DocId, f32)>, Box<dyn std::error::Error>> {
    bufs.ensure_capacity(stream, num_dim as usize, num_docs as usize)?;

    let query_f16: Vec<half::f16> = query.iter().map(|&v| half::f16::from_f32(v)).collect();
    stream.memcpy_htod(&query_f16, &mut bufs.query_f16)?;
    stream.memset_zeros(&mut bufs.distances_f32)?;

    let func = kernels.select_f16_kernel(metric);
    let cfg = launch_config(num_docs);
    let mut builder = stream.launch_builder(func);
    builder.arg(vectors_gpu);
    builder.arg(&bufs.query_f16);
    builder.arg(&mut bufs.distances_f32);
    builder.arg(&num_docs);
    builder.arg(&num_dim);
    unsafe { builder.launch(cfg) }?;

    stream.synchronize()?;
    let n = num_docs as usize;
    stream.memcpy_dtoh(&bufs.distances_f32.slice(..n), &mut bufs.distances_host[..n])?;

    Ok(cpu_topk(&bufs.distances_host[..n], k, metric.higher_is_better()))
}

/// Dispatch RaBitQ binary distance computation on GPU.
pub fn dispatch_vec_search_rabitq(
    stream: &Arc<CudaStream>,
    kernels: &VectorKernelSet,
    codes_gpu: &CudaSlice<u32>,
    norms_gpu: &CudaSlice<f32>,
    query: &[f32],
    quantizer: &RaBitQuantizer,
    num_docs: u32,
    k: usize,
    rerank_factor: usize,
    vectors_cpu: &[f32],
    metric: &VecDist,
    bufs: &mut GpuSearchBuffers,
) -> Result<Vec<(DocId, f32)>, Box<dyn std::error::Error>> {
    bufs.ensure_capacity(stream, query.len(), num_docs as usize)?;

    let (query_code, query_norm) = quantizer.prepare_query(query);
    let query_words = RaBitQuantizer::codes_to_u32_words(&query_code, quantizer.num_dim);
    let words_per_code = quantizer.words_per_code() as u32;

    stream.memcpy_htod(&query_words, &mut bufs.query_u32)?;
    stream.memset_zeros(&mut bufs.distances_f32)?;

    let cfg = launch_config(num_docs);
    let mut builder = stream.launch_builder(&kernels.rabitq_distance);
    builder.arg(codes_gpu);
    builder.arg(norms_gpu);
    builder.arg(&bufs.query_u32);
    builder.arg(&mut bufs.distances_f32);
    builder.arg(&num_docs);
    builder.arg(&words_per_code);
    builder.arg(&query_norm);
    unsafe { builder.launch(cfg) }?;

    stream.synchronize()?;
    let n = num_docs as usize;
    stream.memcpy_dtoh(&bufs.distances_f32.slice(..n), &mut bufs.distances_host[..n])?;

    // Get top candidates (more than k for re-ranking)
    let candidates = cpu_topk(&bufs.distances_host[..n], k * rerank_factor, false); // RaBitQ: lower = closer

    // Re-rank with full-precision distances on CPU
    Ok(rerank_with_f32(
        &candidates,
        query,
        vectors_cpu,
        quantizer.num_dim,
        metric,
        k,
    ))
}

/// Dispatch filtered f32 vector distance computation on GPU.
pub fn dispatch_vec_search_filtered(
    stream: &Arc<CudaStream>,
    kernels: &VectorKernelSet,
    vectors_gpu: &CudaSlice<f32>,
    query: &[f32],
    filter_bitmap: &[u64],
    num_docs: u32,
    num_dim: u32,
    metric: &VecDist,
    k: usize,
    bufs: &mut GpuSearchBuffers,
) -> Result<Vec<(DocId, f32)>, Box<dyn std::error::Error>> {
    bufs.ensure_capacity(stream, num_dim as usize, num_docs as usize)?;

    stream.memcpy_htod(query, &mut bufs.query_f32)?;
    stream.memcpy_htod(filter_bitmap, &mut bufs.filter_bitmap_gpu)?;

    let sentinel = if metric.higher_is_better() {
        f32::NEG_INFINITY
    } else {
        f32::INFINITY
    };

    stream.memset_zeros(&mut bufs.distances_f32)?;

    let func = kernels.select_filtered_kernel(metric);
    let cfg = launch_config(num_docs);
    let mut builder = stream.launch_builder(func);
    builder.arg(vectors_gpu);
    builder.arg(&bufs.query_f32);
    builder.arg(&bufs.filter_bitmap_gpu);
    builder.arg(&mut bufs.distances_f32);
    builder.arg(&num_docs);
    builder.arg(&num_dim);
    builder.arg(&sentinel);
    unsafe { builder.launch(cfg) }?;

    stream.synchronize()?;
    let n = num_docs as usize;
    stream.memcpy_dtoh(&bufs.distances_f32.slice(..n), &mut bufs.distances_host[..n])?;

    // Filter out sentinel values in top-K
    let mut results = cpu_topk(&bufs.distances_host[..n], k, metric.higher_is_better());
    results.retain(|&(_, d)| d.is_finite());
    Ok(results)
}

/// CPU top-K selection via partial sort (O(N) average via select_nth_unstable).
fn cpu_topk(distances: &[f32], k: usize, higher_is_better: bool) -> Vec<(DocId, f32)> {
    let mut indexed: Vec<(DocId, f32)> = distances
        .iter()
        .enumerate()
        .map(|(i, &d)| (i as DocId, d))
        .collect();

    if indexed.len() <= k {
        if higher_is_better {
            indexed.sort_unstable_by(|a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
        } else {
            indexed.sort_unstable_by(|a, b| {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        return indexed;
    }

    // Partition: O(N) average to find top-K boundary
    if higher_is_better {
        indexed.select_nth_unstable_by(k, |a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        indexed.select_nth_unstable_by(k, |a, b| {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    indexed.truncate(k);

    // Sort only the K results
    if higher_is_better {
        indexed.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        indexed.sort_unstable_by(|a, b| {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    indexed
}

/// Re-rank RaBitQ candidates with full-precision f32 distances on CPU.
fn rerank_with_f32(
    candidates: &[(DocId, f32)],
    query: &[f32],
    vectors: &[f32],
    num_dim: u16,
    metric: &VecDist,
    final_k: usize,
) -> Vec<(DocId, f32)> {
    let dim = num_dim as usize;
    let mut rescored: Vec<(DocId, f32)> = candidates
        .iter()
        .map(|&(doc_id, _)| {
            let offset = doc_id as usize * dim;
            let doc_vec = &vectors[offset..offset + dim];
            (doc_id, metric.compute(query, doc_vec))
        })
        .collect();

    if metric.higher_is_better() {
        rescored.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        rescored.sort_unstable_by(|a, b| {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    rescored.truncate(final_k);
    rescored
}

/// Convert f32 vectors to f16 for GPU upload.
pub fn f32_to_f16(vectors: &[f32]) -> Vec<half::f16> {
    vectors.iter().map(|&v| half::f16::from_f32(v)).collect()
}

/// Pre-compute L2 norms for each document vector (for cuBLAS cosine normalization).
pub fn compute_doc_norms(vectors: &[f32], num_docs: u32, num_dim: u32) -> Vec<f32> {
    let dim = num_dim as usize;
    (0..num_docs as usize)
        .map(|i| {
            let offset = i * dim;
            vectors[offset..offset + dim]
                .iter()
                .map(|x| x * x)
                .sum::<f32>()
                .sqrt()
        })
        .collect()
}

/// Dispatch f32 vector search using cuBLAS sgemv for batch dot products.
/// Supports Cosine and DotProduct metrics. Falls back to custom kernel for L2.
pub fn dispatch_vec_search_f32_cublas(
    stream: &Arc<CudaStream>,
    blas: &CudaBlas,
    vectors_gpu: &CudaSlice<f32>,
    query: &[f32],
    doc_norms: Option<&[f32]>,
    num_docs: u32,
    num_dim: u32,
    metric: &VecDist,
    k: usize,
    bufs: &mut GpuSearchBuffers,
) -> Result<Vec<(DocId, f32)>, Box<dyn std::error::Error>> {
    bufs.ensure_capacity(stream, num_dim as usize, num_docs as usize)?;

    stream.memcpy_htod(query, &mut bufs.query_f32)?;
    stream.memset_zeros(&mut bufs.distances_f32)?;

    // cuBLAS sgemv: dots = vectors^T * query
    let cfg = GemvConfig {
        trans: cudarc::cublas::sys::cublasOperation_t::CUBLAS_OP_T,
        m: num_dim as i32,
        n: num_docs as i32,
        alpha: 1.0f32,
        lda: num_dim as i32,
        incx: 1,
        beta: 0.0f32,
        incy: 1,
    };

    unsafe { blas.gemv(cfg, vectors_gpu, &bufs.query_f32, &mut bufs.distances_f32) }?;

    stream.synchronize()?;
    let n = num_docs as usize;
    stream.memcpy_dtoh(&bufs.distances_f32.slice(..n), &mut bufs.distances_host[..n])?;

    match metric {
        VecDist::DotProduct => Ok(cpu_topk(&bufs.distances_host[..n], k, true)),
        VecDist::Cosine => {
            let doc_norms =
                doc_norms.expect("doc_norms required for cuBLAS cosine search");
            let query_norm: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();

            let cosines: Vec<f32> = bufs.distances_host[..n]
                .iter()
                .zip(doc_norms)
                .map(|(&dot, &dn)| {
                    let denom = dn * query_norm;
                    if denom == 0.0 { 0.0 } else { dot / denom }
                })
                .collect();

            Ok(cpu_topk(&cosines, k, true))
        }
        VecDist::Euclidean => {
            let doc_norms =
                doc_norms.expect("doc_norms required for cuBLAS L2 search");
            let query_norm_sq: f32 = query.iter().map(|x| x * x).sum();

            let l2_dists: Vec<f32> = bufs.distances_host[..n]
                .iter()
                .zip(doc_norms)
                .map(|(&dot, &dn)| {
                    let sq = dn * dn + query_norm_sq - 2.0 * dot;
                    sq.max(0.0).sqrt()
                })
                .collect();

            Ok(cpu_topk(&l2_dists, k, false))
        }
    }
}

/// Dispatch f16 vector search using cuBLAS gemm (n=1) for batch dot products.
/// Uses Tensor Cores on Ada/Blackwell GPUs.
pub fn dispatch_vec_search_f16_cublas(
    stream: &Arc<CudaStream>,
    blas: &CudaBlas,
    vectors_gpu: &CudaSlice<half::f16>,
    query: &[f32],
    doc_norms: Option<&[f32]>,
    num_docs: u32,
    num_dim: u32,
    metric: &VecDist,
    k: usize,
    bufs: &mut GpuSearchBuffers,
) -> Result<Vec<(DocId, f32)>, Box<dyn std::error::Error>> {
    use cudarc::cublas::sys::cublasOperation_t;

    bufs.ensure_capacity(stream, num_dim as usize, num_docs as usize)?;

    let query_f16: Vec<half::f16> = query.iter().map(|&v| half::f16::from_f32(v)).collect();
    stream.memcpy_htod(&query_f16, &mut bufs.query_f16)?;
    stream.memset_zeros(&mut bufs.dots_f16)?;

    let cfg = cudarc::cublas::GemmConfig {
        transa: cublasOperation_t::CUBLAS_OP_T,
        transb: cublasOperation_t::CUBLAS_OP_N,
        m: num_docs as i32,
        n: 1,
        k: num_dim as i32,
        alpha: half::f16::from_f32(1.0),
        lda: num_dim as i32,
        ldb: num_dim as i32,
        beta: half::f16::from_f32(0.0),
        ldc: num_docs as i32,
    };

    unsafe {
        cudarc::cublas::Gemm::gemm(blas, cfg, vectors_gpu, &bufs.query_f16, &mut bufs.dots_f16)?;
    }

    stream.synchronize()?;
    let n = num_docs as usize;
    let dots_f16: Vec<half::f16> = stream.clone_dtoh(&bufs.dots_f16.slice(..n))?;
    let dots: Vec<f32> = dots_f16.iter().map(|v| v.to_f32()).collect();

    match metric {
        VecDist::DotProduct => Ok(cpu_topk(&dots, k, true)),
        VecDist::Cosine => {
            let doc_norms =
                doc_norms.expect("doc_norms required for cuBLAS cosine search");
            let query_norm: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();

            let cosines: Vec<f32> = dots
                .iter()
                .zip(doc_norms)
                .map(|(&dot, &dn)| {
                    let denom = dn * query_norm;
                    if denom == 0.0 { 0.0 } else { dot / denom }
                })
                .collect();

            Ok(cpu_topk(&cosines, k, true))
        }
        VecDist::Euclidean => {
            let doc_norms =
                doc_norms.expect("doc_norms required for cuBLAS L2 search");
            let query_norm_sq: f32 = query.iter().map(|x| x * x).sum();

            let l2_dists: Vec<f32> = dots
                .iter()
                .zip(doc_norms)
                .map(|(&dot, &dn)| {
                    let sq = dn * dn + query_norm_sq - 2.0 * dot;
                    sq.max(0.0).sqrt()
                })
                .collect();

            Ok(cpu_topk(&l2_dists, k, false))
        }
    }
}
