#include "bloom.hlsli"

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    int2 pixel = int2(dispatch_id.xy);
    if (any(pixel >= destination_extent())) {
        return;
    }
    int2 source_end = texture_extent(source_image) - 1;
    float3 color = max(source_image.Load(int3(min(pixel, source_end), 0)).rgb, 0.0);
    float peak = max(max(color.r, color.g), color.b);
    float contribution = max(peak - 1.0, 0.0) / max(peak, 1.0);
    destination_image[pixel] = float4(color * contribution, 0.0);
}
