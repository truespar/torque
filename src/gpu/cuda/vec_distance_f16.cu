// vec_distance_f16.cu — f16 vector distance using half2 packed operations.
// Processes 2 elements at a time for better throughput on Ada/Blackwell.

#include <cuda_fp16.h>

extern "C" __global__
void vec_cosine_f16(
    const half* __restrict__ vectors,    // [num_docs x num_dim]
    const half* __restrict__ query,      // [num_dim]
    float* __restrict__ distances,       // [num_docs] (output in f32)
    unsigned int num_docs,
    unsigned int num_dim
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int offset = doc_id * num_dim;
    float dot = 0.0f, norm_a = 0.0f, norm_b = 0.0f;

    // Process pairs using half2 for better throughput
    unsigned int pairs = num_dim / 2;
    for (unsigned int i = 0; i < pairs; i++) {
        half2 a = *reinterpret_cast<const half2*>(&vectors[offset + i * 2]);
        half2 b = *reinterpret_cast<const half2*>(&query[i * 2]);
        float2 af = __half22float2(a);
        float2 bf = __half22float2(b);
        dot += af.x * bf.x + af.y * bf.y;
        norm_a += af.x * af.x + af.y * af.y;
        norm_b += bf.x * bf.x + bf.y * bf.y;
    }

    // Handle odd trailing element
    if (num_dim & 1) {
        float a = __half2float(vectors[offset + num_dim - 1]);
        float b = __half2float(query[num_dim - 1]);
        dot += a * b;
        norm_a += a * a;
        norm_b += b * b;
    }

    float denom = sqrtf(norm_a) * sqrtf(norm_b);
    distances[doc_id] = (denom > 0.0f) ? (dot / denom) : 0.0f;
}

extern "C" __global__
void vec_dot_f16(
    const half* __restrict__ vectors,
    const half* __restrict__ query,
    float* __restrict__ distances,
    unsigned int num_docs,
    unsigned int num_dim
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    unsigned int offset = doc_id * num_dim;
    float dot = 0.0f;

    unsigned int pairs = num_dim / 2;
    for (unsigned int i = 0; i < pairs; i++) {
        half2 a = *reinterpret_cast<const half2*>(&vectors[offset + i * 2]);
        half2 b = *reinterpret_cast<const half2*>(&query[i * 2]);
        float2 af = __half22float2(a);
        float2 bf = __half22float2(b);
        dot += af.x * bf.x + af.y * bf.y;
    }

    if (num_dim & 1) {
        dot += __half2float(vectors[offset + num_dim - 1]) * __half2float(query[num_dim - 1]);
    }

    distances[doc_id] = dot;
}
