// vec_rabitq.cu — RaBitQ binary distance using XOR + hardware __popc.
// Each thread computes the approximate distance for one document
// using hamming distance on binary codes with norm correction.

extern "C" __global__
void vec_rabitq_distance(
    const unsigned int* __restrict__ binary_codes,  // [num_docs x words_per_code]
    const float* __restrict__ norms,                // [num_docs] correction scalars
    const unsigned int* __restrict__ query_binary,  // [words_per_code]
    float* __restrict__ distances,                  // [num_docs]
    unsigned int num_docs,
    unsigned int words_per_code,
    float query_norm
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int offset = doc_id * words_per_code;
    unsigned int hamming = 0;

    for (unsigned int i = 0; i < words_per_code; i++) {
        unsigned int xor_val = binary_codes[offset + i] ^ query_binary[i];
        hamming += __popc(xor_val);  // native hardware popcount
    }

    distances[doc_id] = (float)hamming * norms[doc_id] * query_norm;
}
