// geo_radius.cu — Haversine geo filter kernel
// Each thread checks if a document's (lat, lng) is within radius_km
// of (center_lat, center_lng) and sets the bit in result_bitmap.

#define DEG_TO_RAD 0.017453292519943295f  // PI / 180
#define EARTH_RADIUS_KM 6371.0f

extern "C" __global__
void geo_radius(
    const float* __restrict__ lats,
    const float* __restrict__ lngs,
    unsigned long long* __restrict__ result_bitmap,
    unsigned int num_docs,
    float center_lat,
    float center_lng,
    float radius_km
) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < num_docs) {
        float lat1 = center_lat * DEG_TO_RAD;
        float lat2 = lats[i] * DEG_TO_RAD;
        float dlat = (lats[i] - center_lat) * DEG_TO_RAD;
        float dlng = (lngs[i] - center_lng) * DEG_TO_RAD;

        float sin_dlat = __sinf(dlat * 0.5f);
        float sin_dlng = __sinf(dlng * 0.5f);

        float a = sin_dlat * sin_dlat
                + __cosf(lat1) * __cosf(lat2) * sin_dlng * sin_dlng;

        // 2 * atan2(sqrt(a), sqrt(1-a)) — use asinf for numerical stability
        float c = 2.0f * asinf(fminf(sqrtf(a), 1.0f));
        float dist = EARTH_RADIUS_KM * c;

        if (dist <= radius_km) {
            unsigned int word_idx = i / 64;
            unsigned long long bit = 1ULL << (i % 64);
            atomicOr(&result_bitmap[word_idx], bit);
        }
    }
}
