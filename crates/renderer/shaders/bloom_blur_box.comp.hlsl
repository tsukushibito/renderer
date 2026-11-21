#include "bloom.hlsli"

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    int2 pixel = int2(dispatch_id.xy);
    if (any(pixel >= destination_extent())) {
        return;
    }
    int2 source_end = texture_extent(source_image) - 1;
    float3 sum = 0.0;
    for (int y = -1; y <= 1; ++y) {
        for (int x = -1; x <= 1; ++x) {
            int2 sample_pixel = clamp(pixel + int2(x, y), 0, source_end);
            sum += source_image.Load(int3(sample_pixel, 0)).rgb;
        }
    }
    destination_image[pixel] = float4(sum * (1.0 / 9.0), 0.0);
}
