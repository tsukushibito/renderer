#include "bloom.hlsli"

#ifdef BLOOM_EXTRACT
float3 truncate_to_half_grid(float3 value) {
    // The nonnegative finite extraction result fits binary16. Drop the
    // extra FP32 significand bits, using the fixed binary16 subnormal step
    // below its smallest normal. This keeps the previous target's
    // round-toward-zero result on both backends.
    const float minimum_normal = 1.0 / 16384.0;
    const float subnormal_scale = 16777216.0;
    float3 normal = asfloat(asuint(value) & 0xffffe000u);
    float3 subnormal = trunc(value * subnormal_scale) / subnormal_scale;
    return select(value < minimum_normal, subnormal, normal);
}
#endif

float3 color_at(int2 pixel) {
    float3 color = source_image.Load(int3(pixel, 0)).rgb;
#ifdef BLOOM_EXTRACT
    color = max(color, 0.0);
    float peak = max(max(color.r, color.g), color.b);
    float contribution = max(peak - 1.0, 0.0) / max(peak, 1.0);
    color = truncate_to_half_grid(color * contribution);
#endif
    return color;
}

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    int2 pixel = int2(dispatch_id.xy);
    if (any(pixel >= destination_extent())) {
        return;
    }
    int2 source_end = texture_extent(source_image) - 1;
    int2 first = min(pixel * 2, source_end);
    int2 last = min(first + 1, source_end);
    float3 sum = color_at(first)
               + color_at(int2(last.x, first.y))
               + color_at(int2(first.x, last.y))
               + color_at(last);
    destination_image[pixel] = float4(sum * 0.25, 0.0);
}
