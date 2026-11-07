pub mod bitmap;
#[cfg(feature = "cuda")]
pub mod kernels;

#[cfg(feature = "cuda")]
use cudarc::cublas::CudaBlas;
#[cfg(feature = "cuda")]
use cudarc::driver::{CudaContext, CudaModule, CudaStream};
#[cfg(feature = "cuda")]
use std::sync::{Arc, Mutex};

/// GPU compute context holding CUDA device, stream, and loaded kernel module.
#[cfg(feature = "cuda")]
pub struct GpuContext {
    pub ctx: Arc<CudaContext>,
    pub stream: Arc<CudaStream>,
    pub module: Arc<CudaModule>,
    pub blas: CudaBlas,
    pub device_name: String,
    pub vram_bytes: u64,
    /// VRAM budget for dense bitmaps in bytes. 0 = auto (50% of device VRAM).
    pub bitmap_vram_budget: u64,
    pub sm_version: u32,
    pub vector_kernels: Option<crate::vector::gpu::VectorKernelSet>,
    pub search_buffers: Mutex<crate::vector::gpu::GpuSearchBuffers>,
}

#[cfg(feature = "cuda")]
impl GpuContext {
    /// Try to initialize a CUDA GPU context on device 0.
    /// `vram_budget` is the VRAM budget for dense bitmaps in bytes (0 = auto-detect 50% of device VRAM).
    /// Returns None if no GPU is available or CUDA init fails.
    pub fn try_init(vram_budget: u64) -> Option<Self> {
        let ctx = match CudaContext::new(0) {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::warn!("CUDA init failed: {e}");
                return None;
            }
        };

        let device_name = ctx.name().unwrap_or_else(|_| "unknown".into());
        let (major, minor) = ctx
            .compute_capability()
            .unwrap_or((0, 0));
        let sm_version = (major * 10 + minor) as u32;
        let vram_bytes = get_total_vram(&ctx).unwrap_or(0);
        let stream = ctx.default_stream();

        let module = match load_kernels(&ctx, sm_version) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("Failed to load CUDA kernels: {e}");
                return None;
            }
        };

        let blas = match CudaBlas::new(stream.clone()) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to create cuBLAS handle: {e}");
                return None;
            }
        };

        let vector_kernels = match crate::vector::gpu::VectorKernelSet::load(&module) {
            Ok(vk) => {
                tracing::info!("Loaded vector GPU kernels");
                Some(vk)
            }
            Err(e) => {
                tracing::warn!("Failed to load vector kernels: {e}");
                None
            }
        };

        // Start with modest buffers — ensure_capacity() will grow on demand
        let initial_dim = 256;
        let initial_docs = 10_000;
        let search_buffers = match crate::vector::gpu::GpuSearchBuffers::new(&stream, initial_dim, initial_docs) {
            Ok(b) => {
                tracing::info!(dim = initial_dim, docs = initial_docs, "Pre-allocated GPU search buffers (will grow on demand)");
                b
            }
            Err(e) => {
                tracing::error!("Failed to allocate GPU search buffers: {e}");
                return None;
            }
        };

        // Resolve bitmap VRAM budget: 0 = auto (50% of device VRAM)
        let bitmap_vram_budget = if vram_budget > 0 {
            vram_budget
        } else {
            vram_bytes / 2
        };

        tracing::info!(
            device = %device_name,
            sm = sm_version,
            vram_mb = vram_bytes / (1024 * 1024),
            bitmap_budget_mb = bitmap_vram_budget / (1024 * 1024),
            "GPU initialized (with cuBLAS)"
        );

        Some(Self {
            ctx,
            stream,
            module,
            blas,
            device_name,
            vram_bytes,
            bitmap_vram_budget,
            sm_version,
            vector_kernels,
            search_buffers: Mutex::new(search_buffers),
        })
    }
}

/// Get total VRAM in bytes via cuMemGetInfo.
#[cfg(feature = "cuda")]
fn get_total_vram(ctx: &Arc<CudaContext>) -> Result<u64, cudarc::driver::DriverError> {
    ctx.bind_to_thread()?;
    let (_, total) = cudarc::driver::result::mem_get_info()?;
    Ok(total as u64)
}

/// Load CUDA kernels: try pre-compiled PTX from build.rs, fall back to runtime compilation.
#[cfg(feature = "cuda")]
fn load_kernels(
    ctx: &Arc<CudaContext>,
    sm_version: u32,
) -> Result<Arc<CudaModule>, Box<dyn std::error::Error>> {
    // Try loading pre-compiled PTX from build.rs output
    if let Some(module) = try_load_precompiled(ctx, sm_version) {
        tracing::info!("Loaded pre-compiled CUDA kernels");
        return Ok(module);
    }

    // Fall back to runtime compilation via nvrtc
    tracing::info!("Compiling CUDA kernels at runtime via nvrtc");
    let module = compile_kernels_runtime(ctx)?;
    Ok(module)
}

/// Pre-compiled PTX embedded at compile time for each SM target.
/// build.rs compiles combined .cu → kernels_sm_XX.ptx for each target arch.
#[cfg(feature = "cuda")]
struct PrecompiledPtx {
    sm: u32,
    ptx: &'static str,
}

#[cfg(feature = "cuda")]
const PRECOMPILED: &[PrecompiledPtx] = &[
    PrecompiledPtx {
        sm: 80,
        ptx: include_str!(concat!(env!("OUT_DIR"), "/kernels_sm_80.ptx")),
    },
    PrecompiledPtx {
        sm: 86,
        ptx: include_str!(concat!(env!("OUT_DIR"), "/kernels_sm_86.ptx")),
    },
    PrecompiledPtx {
        sm: 89,
        ptx: include_str!(concat!(env!("OUT_DIR"), "/kernels_sm_89.ptx")),
    },
    PrecompiledPtx {
        sm: 120,
        ptx: include_str!(concat!(env!("OUT_DIR"), "/kernels_sm_120.ptx")),
    },
];

/// Try to load pre-compiled PTX matching the detected GPU's SM version.
/// Falls back to the closest lower SM version if an exact match isn't found.
#[cfg(feature = "cuda")]
fn try_load_precompiled(ctx: &Arc<CudaContext>, sm_version: u32) -> Option<Arc<CudaModule>> {
    let marker = include_str!(concat!(env!("OUT_DIR"), "/cuda_kernels_compiled"));
    if marker == "false" {
        return None;
    }

    // Find best matching PTX: exact match or closest lower SM version
    let best = PRECOMPILED
        .iter()
        .filter(|p| p.sm <= sm_version)
        .max_by_key(|p| p.sm)?;

    tracing::info!(target_sm = best.sm, gpu_sm = sm_version, "Loading pre-compiled PTX");
    let ptx = cudarc::nvrtc::Ptx::from_src(best.ptx);
    ctx.load_module(ptx).ok()
}

/// Compile CUDA kernels at runtime via nvrtc (fallback when nvcc wasn't available at build time).
/// Note: f16 kernels are excluded because nvrtc may not find cuda_fp16.h.
#[cfg(feature = "cuda")]
fn compile_kernels_runtime(
    ctx: &Arc<CudaContext>,
) -> Result<Arc<CudaModule>, Box<dyn std::error::Error>> {
    let kernel_sources = get_kernel_sources();
    let combined = kernel_sources.join("\n\n");
    // f16 kernels omitted — require cuda_fp16.h which nvrtc may not find

    let (major, minor) = ctx.compute_capability().unwrap_or((8, 9));
    let arch: &'static str = match (major, minor) {
        (12, 0) => "sm_120",
        (8, 9) => "sm_89",
        (8, _) => "sm_80",
        (9, _) => "sm_90",
        _ => "sm_89",
    };

    let ptx = cudarc::nvrtc::compile_ptx_with_opts(
        &combined,
        cudarc::nvrtc::CompileOptions {
            arch: Some(arch),
            use_fast_math: Some(true),
            ..Default::default()
        },
    )?;

    let module = ctx.load_module(ptx)?;
    Ok(module)
}

/// Get all CUDA kernel source code as strings.
#[cfg(feature = "cuda")]
fn get_kernel_sources() -> Vec<&'static str> {
    vec![
        include_str!("cuda/bitmap_and.cu"),
        include_str!("cuda/multi_and.cu"),
        include_str!("cuda/bitmap_or.cu"),
        include_str!("cuda/range_filter.cu"),
        include_str!("cuda/geo_radius.cu"),
        include_str!("cuda/bm25f.cu"),
        include_str!("cuda/facet_count.cu"),
        include_str!("cuda/vec_distance.cu"),
        include_str!("cuda/vec_rabitq.cu"),
        include_str!("cuda/vec_filtered.cu"),
        include_str!("cuda/vec_topk.cu"),
    ]
}

