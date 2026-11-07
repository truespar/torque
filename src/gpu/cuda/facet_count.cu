// facet_count.cu — Parallel popcount facet counting kernel
// For each facet, count the number of set bits in (result_bitmap AND facet_bitmap).
// Uses __popcll() for per-word popcount and warp-level reduction via __shfl_down_sync.
//
// Grid: one block per facet, blockDim.x threads per block.
// Each thread handles multiple words, then warp-reduces the sum.

extern "C" __global__
void facet_count(
    const unsigned long long* __restrict__ result_bitmap,
    const unsigned long long* __restrict__ facet_bitmaps,
    unsigned int* __restrict__ counts,
    unsigned int words_per_bitmap,
    unsigned int num_facets
) {
    unsigned int facet_id = blockIdx.x;
    if (facet_id >= num_facets) return;

    unsigned int tid = threadIdx.x;
    unsigned int stride = blockDim.x;

    const unsigned long long* facet_base = facet_bitmaps + (unsigned long long)facet_id * words_per_bitmap;

    // Each thread sums popcount for its assigned words
    unsigned int local_count = 0;
    for (unsigned int w = tid; w < words_per_bitmap; w += stride) {
        unsigned long long masked = result_bitmap[w] & facet_base[w];
        local_count += __popcll(masked);
    }

    // Warp-level reduction via shuffle
    unsigned int mask = 0xFFFFFFFF;
    for (int offset = 16; offset > 0; offset >>= 1) {
        local_count += __shfl_down_sync(mask, local_count, offset);
    }

    // Lane 0 of each warp writes to shared memory
    __shared__ unsigned int warp_sums[32];  // max 32 warps per block (1024 threads)
    unsigned int warp_id = tid / 32;
    unsigned int lane_id = tid % 32;

    if (lane_id == 0) {
        warp_sums[warp_id] = local_count;
    }
    __syncthreads();

    // First warp reduces across all warp sums
    if (warp_id == 0) {
        unsigned int num_warps = (blockDim.x + 31) / 32;
        unsigned int val = (lane_id < num_warps) ? warp_sums[lane_id] : 0;
        for (int offset = 16; offset > 0; offset >>= 1) {
            val += __shfl_down_sync(mask, val, offset);
        }
        if (lane_id == 0) {
            counts[facet_id] = val;
        }
    }
}
