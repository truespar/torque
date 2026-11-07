use cudarc::driver::{CudaFunction, CudaModule, LaunchConfig};
use std::sync::Arc;

/// All CUDA kernel function handles, loaded from a single compiled module.
pub struct KernelSet {
    pub bitmap_and: CudaFunction,
    pub multi_and: CudaFunction,
    pub bitmap_or: CudaFunction,
    pub range_filter: CudaFunction,
    pub geo_radius: CudaFunction,
    pub bm25f_score: CudaFunction,
    pub facet_count: CudaFunction,
}

impl KernelSet {
    /// Load all kernel functions from a compiled CUDA module.
    pub fn load(module: &Arc<CudaModule>) -> Result<Self, cudarc::driver::DriverError> {
        Ok(Self {
            bitmap_and: module.load_function("bitmap_and")?,
            multi_and: module.load_function("multi_and")?,
            bitmap_or: module.load_function("bitmap_or")?,
            range_filter: module.load_function("range_filter")?,
            geo_radius: module.load_function("geo_radius")?,
            bm25f_score: module.load_function("bm25f_score")?,
            facet_count: module.load_function("facet_count")?,
        })
    }
}

/// Build a 1D launch config for the given number of elements.
/// Uses 256 threads per block (good default for compute-bound kernels).
pub fn launch_config(num_elems: u32) -> LaunchConfig {
    let block_dim = 256u32;
    let grid_dim = (num_elems + block_dim - 1) / block_dim;
    LaunchConfig {
        grid_dim: (grid_dim, 1, 1),
        block_dim: (block_dim, 1, 1),
        shared_mem_bytes: 0,
    }
}

/// Build a launch config for facet counting.
/// One block per facet, 256 threads per block for warp-level reduction.
pub fn facet_launch_config(num_facets: u32) -> LaunchConfig {
    LaunchConfig {
        grid_dim: (num_facets, 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_config_small() {
        let cfg = launch_config(100);
        assert_eq!(cfg.block_dim, (256, 1, 1));
        assert_eq!(cfg.grid_dim, (1, 1, 1)); // 100 < 256
    }

    #[test]
    fn launch_config_exact() {
        let cfg = launch_config(256);
        assert_eq!(cfg.grid_dim, (1, 1, 1));
    }

    #[test]
    fn launch_config_large() {
        let cfg = launch_config(1000);
        assert_eq!(cfg.grid_dim, (4, 1, 1)); // ceil(1000/256) = 4
    }

    #[test]
    fn launch_config_one() {
        let cfg = launch_config(1);
        assert_eq!(cfg.grid_dim, (1, 1, 1));
    }

    #[test]
    fn facet_config() {
        let cfg = facet_launch_config(50);
        assert_eq!(cfg.grid_dim, (50, 1, 1));
        assert_eq!(cfg.block_dim, (256, 1, 1));
    }
}
