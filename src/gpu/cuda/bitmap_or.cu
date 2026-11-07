// bitmap_or.cu — Bitmap union kernel
// Each thread ORs one u64 word from bitmap A with bitmap B.

extern "C" __global__
void bitmap_or(
    const unsigned long long* __restrict__ bitmap_store,
    unsigned long long* __restrict__ result,
    unsigned int offset_a,
    unsigned int offset_b,
    unsigned int num_words
) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < num_words) {
        result[i] = bitmap_store[offset_a + i] | bitmap_store[offset_b + i];
    }
}
