#include "local_lights.hlsli"
#include "pbr_math.hlsli"

#if BISTRO_LOCAL_LIGHTS == 2
ByteAddressBuffer tile_lists : register(t3, space0);
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
    uint tile = min(tile_xy.y * local_light_columns(extent) + tile_xy.x, MAX_LOCAL_LIGHT_TILES - 1u);
    uint offset = tile * LOCAL_LIGHT_TILE_WORDS;
    uint count = min(tile_lists.Load(offset * 4u), MAX_LOCAL_LIGHTS);
#else
    uint count = light_count;
#endif
    for (uint entry = 0u; entry < count; ++entry) {
#if BISTRO_LOCAL_LIGHTS == 2
        uint index = tile_lists.Load((offset + 1u + entry) * 4u);
#else
        uint index = entry;
#endif
        if (index < light_count) {
            result += point_light_radiance(index, position, n, v, f0, diffuse_color, roughness);
        }
    }
    return result;
}
