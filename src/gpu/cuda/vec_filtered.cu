// vec_filtered.cu — Filtered vector distance computation.
// Only computes distances for docs whose bit is set in filter_bitmap.
// Filtered-out docs get a sentinel value.

extern "C" __global__
void vec_cosine_filtered(
    const float* __restrict__ vectors,
    const float* __restrict__ query,
    const unsigned long long* __restrict__ filter_bitmap,  // u64 words
    float* __restrict__ distances,
    unsigned int num_docs,
    unsigned int num_dim,
    float sentinel
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    // Check filter bitmap (u64 words)
    unsigned int word_idx = doc_id / 64;
    unsigned int bit_idx = doc_id % 64;
    if (((filter_bitmap[word_idx] >> bit_idx) & 1ULL) == 0ULL) {
        distances[doc_id] = sentinel;
        return;
    }

    unsigned int offset = doc_id * num_dim;
    float dot = 0.0f, norm_a = 0.0f, norm_b = 0.0f;

    for (unsigned int i = 0; i < num_dim; i++) {
        float a = vectors[offset + i];
        float b = query[i];
        dot += a * b;
        norm_a += a * a;
        norm_b += b * b;
    }

    float denom = sqrtf(norm_a) * sqrtf(norm_b);
    distances[doc_id] = (denom > 0.0f) ? (dot / denom) : 0.0f;
}

extern "C" __global__
void vec_dot_filtered(
    const float* __restrict__ vectors,
    const float* __restrict__ query,
    const unsigned long long* __restrict__ filter_bitmap,
    float* __restrict__ distances,
    unsigned int num_docs,
    unsigned int num_dim,
    float sentinel
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int word_idx = doc_id / 64;
    unsigned int bit_idx = doc_id % 64;
    if (((filter_bitmap[word_idx] >> bit_idx) & 1ULL) == 0ULL) {
        distances[doc_id] = sentinel;
        return;
    }

    unsigned int offset = doc_id * num_dim;
    float dot = 0.0f;

    for (unsigned int i = 0; i < num_dim; i++) {
        dot += vectors[offset + i] * query[i];
    }

    distances[doc_id] = dot;
}

extern "C" __global__
void vec_l2_filtered(
    const float* __restrict__ vectors,
    const float* __restrict__ query,
    const unsigned long long* __restrict__ filter_bitmap,
    float* __restrict__ distances,
    unsigned int num_docs,
    unsigned int num_dim,
    float sentinel
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int word_idx = doc_id / 64;
    unsigned int bit_idx = doc_id % 64;
    if (((filter_bitmap[word_idx] >> bit_idx) & 1ULL) == 0ULL) {
        distances[doc_id] = sentinel;
        return;
    }

    unsigned int offset = doc_id * num_dim;
    float sum_sq = 0.0f;

    for (unsigned int i = 0; i < num_dim; i++) {
        float diff = vectors[offset + i] - query[i];
        sum_sq += diff * diff;
    }

    distances[doc_id] = sqrtf(sum_sq);
}
