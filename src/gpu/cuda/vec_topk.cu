// vec_topk.cu — GPU top-K candidate pre-selection.
// Partitions distances into "possibly top-K" candidates using a threshold.
// Each thread checks if its distance qualifies and atomically writes
// to the output candidates array.
//
// Strategy: two-pass approach:
//   Pass 1 (this kernel): find docs that pass a distance threshold
//   Pass 2 (CPU): exact top-K from the candidates

extern "C" __global__
void vec_topk_threshold(
    const float* __restrict__ distances,
    unsigned int* __restrict__ candidate_ids,
    unsigned int* __restrict__ candidate_count,
    unsigned int num_docs,
    float threshold,
    unsigned int max_candidates,
    int higher_is_better  // 1 for cosine/dot, 0 for L2
) {
    unsigned int doc_id = blockIdx.x * blockDim.x + threadIdx.x;
    if (doc_id >= num_docs) return;

    float dist = distances[doc_id];
    int qualifies;

    if (higher_is_better) {
        qualifies = (dist >= threshold);
    } else {
        qualifies = (dist <= threshold && dist >= 0.0f);
    }

    if (qualifies) {
        unsigned int idx = atomicAdd(candidate_count, 1);
        if (idx < max_candidates) {
            candidate_ids[idx] = doc_id;
        }
    }
}
