#include "grid.hlsli"
Texture2D<float> depth_image : register(t1);
StructuredBuffer<uint> page_table : register(t2);
Texture2D<float> shadow_atlas : register(t3);
#ifdef __spirv__
[[vk::image_format("rgba16f")]]
#endif
RWTexture2D<float4> visibility : register(u4);
int2 virtual_origin(uint level) { return page_origin[level].xy; }
uint virtual_page_entry(uint level, uint2 page) { return page_table[(level * 128 + page.y) * 128 + page.x]; }
SamplerComparisonState shadow_compare : register(s5);
float virtual_compare(float2 uv, float reference) {
    return shadow_atlas.SampleCmpLevelZero(shadow_compare, uv, reference);
}
#include "virtual_shadow_lookup.hlsli"

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
            value = virtual_visibility(world.xyz / world.w, cross(light_x.xyz, light_y.xyz),
                eye_distance, light_x.xyz, light_y.xyz, depth_plane, shadow_bias);
        }
    }
    visibility[id.xy] = float4(value.xxx, 1.0);
}
