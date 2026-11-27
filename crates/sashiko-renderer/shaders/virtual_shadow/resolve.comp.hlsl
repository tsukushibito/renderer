#include "grid.hlsli"
Texture2D<float> depth_image : register(t1);
StructuredBuffer<uint> page_table : register(t2);
Texture2D<float> shadow_atlas : register(t3);
#ifdef __spirv__
[[vk::image_format("rgba16f")]]
#endif
RWTexture2D<float4> visibility : register(u4);
#include "lookup.hlsli"

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint2 extent, source, atlas_size;
    visibility.GetDimensions(extent.x, extent.y);
    if (any(id.xy >= extent)) return;
    depth_image.GetDimensions(source.x, source.y);
    shadow_atlas.GetDimensions(atlas_size.x, atlas_size.y);
    float value = 1.0;
    if (all(id.xy < source) && all(atlas_size == 4096)) {
        float z = depth_image.Load(int3(id.xy, 0));
        float4 world = receiver_world(id.xy, source, z);
        if (z >= 0.0 && z < 1.0 && abs(world.w) > 1e-20) {
            // Depth-only inspection has no geometric normals. Use normal
            // incidence; forward material shading supplies its actual normal.
            value = virtual_visibility(world.xyz / world.w, cross(light_x.xyz, light_y.xyz));
        }
    }
    visibility[id.xy] = float4(value.xxx, 1.0);
}
