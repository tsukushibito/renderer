#include "bloom.hlsli"

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    int2 pixel = int2(dispatch_id.xy);
    if (any(pixel >= destination_extent())) {
        return;
    }
    int2 source_end = texture_extent(source_image) - 1;
    int2 first = min(pixel * 2, source_end);
    int2 last = min(first + 1, source_end);
    float3 sum = source_image.Load(int3(first, 0)).rgb
               + source_image.Load(int3(last.x, first.y, 0)).rgb
               + source_image.Load(int3(first.x, last.y, 0)).rgb
               + source_image.Load(int3(last, 0)).rgb;
    destination_image[pixel] = float4(sum * 0.25, 0.0);
}
