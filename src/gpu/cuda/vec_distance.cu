// vec_distance.cu — f32 vector distance kernels (cosine, dot product, L2)
// Each thread computes the distance between one document vector and the query.

extern "C" __global__
void vec_cosine_f32(
    const float* __restrict__ vectors,   // [num_docs x num_dim]
    const float* __restrict__ query,     // [num_dim]
    float* __restrict__ distances,       // [num_docs]
    unsigned int num_docs,
    unsigned int num_dim
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

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
void vec_dot_f32(
    const float* __restrict__ vectors,
    const float* __restrict__ query,
    float* __restrict__ distances,
    unsigned int num_docs,
    unsigned int num_dim
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int offset = doc_id * num_dim;
    float dot = 0.0f;

    for (unsigned int i = 0; i < num_dim; i++) {
        dot += vectors[offset + i] * query[i];
    }

    distances[doc_id] = dot;
}

extern "C" __global__
void vec_l2_f32(
    const float* __restrict__ vectors,
    const float* __restrict__ query,
    float* __restrict__ distances,
    unsigned int num_docs,
    unsigned int num_dim
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int offset = doc_id * num_dim;
    float sum_sq = 0.0f;

    for (unsigned int i = 0; i < num_dim; i++) {
        float diff = vectors[offset + i] - query[i];
        sum_sq += diff * diff;
    }

    distances[doc_id] = sqrtf(sum_sq);
}
