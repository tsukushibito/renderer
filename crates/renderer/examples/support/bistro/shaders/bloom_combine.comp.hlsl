#include "bloom.hlsli"

Texture2D<float4> blur_image : register(t1, space0);

float3 blur_at(float2 coordinate, int2 extent) {
    float2 position = clamp(coordinate, 0.0, float2(extent - 1));
    int2 lower = int2(position);
    int2 upper = min(lower + 1, extent - 1);
    float2 fraction = position - float2(lower);
    float3 top = lerp(blur_image.Load(int3(lower, 0)).rgb,
                      blur_image.Load(int3(upper.x, lower.y, 0)).rgb, fraction.x);
    float3 bottom = lerp(blur_image.Load(int3(lower.x, upper.y, 0)).rgb,
                         blur_image.Load(int3(upper, 0)).rgb, fraction.x);
    return lerp(top, bottom, fraction.y);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    int2 pixel = int2(dispatch_id.xy);
    int2 extent = destination_extent();
    if (any(pixel >= extent)) {
        return;
    }
    int2 blur_extent = texture_extent(blur_image);
    float2 position = (float2(pixel) + 0.5) * (float2(blur_extent) / float2(extent)) - 0.5;
    int2 source_end = texture_extent(source_image) - 1;
    float4 color = source_image.Load(int3(min(pixel, source_end), 0));
    float3 combined = min(color.rgb + blur_at(position, blur_extent) * 0.125, 65504.0);
    destination_image[pixel] = float4(combined, color.a);
}
