// bm25f.cu — BM25F scoring kernel
// Each thread scores one document:
//   score = idf * (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * dl / avg_dl))

extern "C" __global__
void bm25f_score(
    const unsigned int* __restrict__ term_freqs,
    const unsigned int* __restrict__ doc_lengths,
    float* __restrict__ scores,
    unsigned int num_docs,
    float avg_dl,
    float idf,
    float k1,
    float b
) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < num_docs) {
        float tf = (float)term_freqs[i];
        float dl = (float)doc_lengths[i];

        if (tf > 0.0f) {
            float norm = 1.0f - b + b * (dl / avg_dl);
            float numerator = tf * (k1 + 1.0f);
            float denominator = tf + k1 * norm;
            scores[i] = idf * (numerator / denominator);
        } else {
            scores[i] = 0.0f;
        }
    }
}
