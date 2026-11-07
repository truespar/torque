// range_filter.cu — Numeric range → bitmap kernel
// Each thread checks one document's value against [min_val, max_val]
// and sets the corresponding bit in the result bitmap via atomicOr.

extern "C" __global__
void range_filter(
    const long long* __restrict__ values,
    unsigned long long* __restrict__ result_bitmap,
    unsigned int num_docs,
    long long min_val,
    long long max_val
) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < num_docs) {
        long long v = values[i];
        if (v >= min_val && v <= max_val) {
            unsigned int word_idx = i / 64;
            unsigned long long bit = 1ULL << (i % 64);
            atomicOr(&result_bitmap[word_idx], bit);
        }
    }
}
