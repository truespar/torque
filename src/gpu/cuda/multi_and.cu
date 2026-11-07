// multi_and.cu — N-way bitmap AND kernel
// Each thread computes the AND of one word position across all N bitmaps.

extern "C" __global__
void multi_and(
    const unsigned long long* __restrict__ bitmap_store,
    const unsigned int* __restrict__ offsets,
    unsigned long long* __restrict__ result,
    unsigned int num_bitmaps,
    unsigned int num_words
) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < num_words) {
        unsigned long long acc = ~0ULL;  // all bits set
        for (unsigned int b = 0; b < num_bitmaps; b++) {
            acc &= bitmap_store[offsets[b] + i];
        }
        result[i] = acc;
    }
}
