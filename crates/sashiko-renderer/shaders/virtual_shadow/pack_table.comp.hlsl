#include "virtual_shadow_math.hlsli"
StructuredBuffer<uint4> entries : register(t0);
#ifdef __spirv__
[[vk::image_format("rgba16f")]]
#endif
RWTexture2D<float4> page_image : register(u1);

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint2 extent;
    page_image.GetDimensions(extent.x, extent.y);
    if (any(id.xy >= extent)) return;
    // Slot+1 (0..961) is exactly representable in binary16.
    float4 value = 0.0;
    if (id.x < 32 && id.y < 1024) value = float4(min(entries[id.y * 32 + id.x], VSM_CAPACITY));
    page_image[id.xy] = value;
}
