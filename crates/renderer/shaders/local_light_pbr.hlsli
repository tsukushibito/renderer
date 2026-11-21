#include "local_lights.hlsli"
#include "pbr_math.hlsli"

#if BISTRO_LOCAL_LIGHTS == 2
Texture2D<float4> tile_masks : register(t3, space0);

uint mask_byte(float value) {
    // A caller can bind an initialized arbitrary float image to the raw
    // program. Reject out-of-range or non-finite values before conversion.
    return (uint)(value >= 0.0 && value <= 255.0 ? value : 0.0);
}

uint read_tile_mask(uint2 texel) {
    float4 bytes = tile_masks.Load(int3(texel, 0));
    return mask_byte(bytes.x) | (mask_byte(bytes.y) << 8u) |
        (mask_byte(bytes.z) << 16u) | (mask_byte(bytes.w) << 24u);
}
#endif

float3 point_light_radiance(
    uint index, float3 position, float3 n, float3 v, float3 f0, float3 diffuse_color, float roughness
) {
    PointLight light = load_point_light(index);
    float3 delta = light.position_radius.xyz - position;
    float squared_distance = dot(delta, delta);
    float radius = light.position_radius.w;
    if (!(radius > 0.0) || !(squared_distance < radius * radius)) { return 0.0; }
    float3 l = unit_or(delta, n);
    float nl = max(dot(n, l), 0.0);
    if (!(nl > 0.0)) { return 0.0; }
    float3 h = unit_or(v + l, n);
    float nv = max(dot(n, v), 0.0);
    float nh = max(dot(n, h), 0.0);
    float vh = max(dot(v, h), 0.0);
    float3 direct_brdf = evaluate_brdf(nl, nv, nh, vh, f0, diffuse_color, roughness);
    float radial = squared_distance / (radius * radius);
    float window = max(1.0 - radial * radial, 0.0);
    float attenuation = window * window / max(squared_distance, 0.01);
    float3 energy = max(light.color_intensity.rgb, 0.0) * max(light.color_intensity.w, 0.0);
    return direct_brdf * energy * nl * attenuation;
}

float3 local_light_radiance(
    float2 pixel_center, float3 position, float3 n, float3 v, float3 f0, float3 diffuse_color, float roughness
) {
    uint light_count = local_light_count();
    float3 result = 0.0;
#if BISTRO_LOCAL_LIGHTS == 2
    uint2 extent = local_light_extent();
    uint2 pixel = uint2(clamp(pixel_center, 0.0, float2(extent - 1u)));
    uint2 tile_xy = pixel / LOCAL_LIGHT_TILE_SIZE;
    uint2 dimensions;
    tile_masks.GetDimensions(dimensions.x, dimensions.y);
    uint columns = (extent.x + LOCAL_LIGHT_TILE_SIZE - 1u) / LOCAL_LIGHT_TILE_SIZE;
    uint words = dimensions.x / columns;
    uint2 texel = uint2(tile_xy.x * words, tile_xy.y);
    if (texel.y >= dimensions.y) { return result; }

    // Visit light IDs in ascending order to preserve accumulation.
    for (uint half_index = 0u; half_index < min(words, (light_count + 31u) / 32u); ++half_index) {
        uint mask = read_tile_mask(texel + uint2(half_index, 0));
        while (mask != 0u) {
            uint index = half_index * 32u + (uint)firstbitlow(mask);
            mask &= mask - 1u;
            if (index < light_count) {
                result += point_light_radiance(index, position, n, v, f0, diffuse_color, roughness);
            }
        }
    }
#else
    for (uint index = 0u; index < light_count; ++index) {
        result += point_light_radiance(index, position, n, v, f0, diffuse_color, roughness);
    }
#endif
    return result;
}
