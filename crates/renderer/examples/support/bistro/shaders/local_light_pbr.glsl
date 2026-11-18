#include "local_lights.glsl"

#if BISTRO_LOCAL_LIGHTS == 2
layout(set = 0, binding = 3, std430) readonly buffer TileLists {
    uint words[];
} tile_lists;
#endif

vec3 point_light_radiance(
    uint index, vec3 position, vec3 n, vec3 v, vec3 f0, vec3 diffuse_color, float roughness
) {
    PointLight light = local_lights.lights[index];
    vec3 delta = light.position_radius.xyz - position;
    float squared_distance = dot(delta, delta);
    float radius = light.position_radius.w;
    if (!(radius > 0.0) || !(squared_distance < radius * radius)) { return vec3(0.0); }
    vec3 l = unit_or(delta, n);
    float nl = max(dot(n, l), 0.0);
    if (!(nl > 0.0)) { return vec3(0.0); }
    vec3 h = unit_or(v + l, n);
    float nv = max(dot(n, v), 0.0);
    float nh = max(dot(n, h), 0.0);
    float vh = max(dot(v, h), 0.0);
    float a = max(roughness * roughness, 1e-3);
    float a2 = a * a;
    float denominator = nh * nh * (a2 - 1.0) + 1.0;
    float distribution = a2 / max(PI * denominator * denominator, 1e-12);
    float visibility_l = nv * sqrt(a2 + (1.0 - a2) * nl * nl);
    float visibility_v = nl * sqrt(a2 + (1.0 - a2) * nv * nv);
    float visibility = 0.5 / max(visibility_l + visibility_v, 1e-6);
    float schlick = pow(clamp(1.0 - vh, 0.0, 1.0), 5.0);
    vec3 fresnel = f0 + (vec3(1.0) - f0) * schlick;
    vec3 diffuse = (vec3(1.0) - fresnel) * diffuse_color / PI;
    vec3 specular = fresnel * distribution * visibility;
    float radial = squared_distance / (radius * radius);
    float window = max(1.0 - radial * radial, 0.0);
    float attenuation = window * window / max(squared_distance, 0.01);
    vec3 energy = max(light.color_intensity.rgb, vec3(0.0)) * max(light.color_intensity.w, 0.0);
    return (diffuse + specular) * energy * nl * attenuation;
}

vec3 local_light_radiance(
    vec3 position, vec3 n, vec3 v, vec3 f0, vec3 diffuse_color, float roughness
) {
    uint light_count = local_light_count();
    vec3 result = vec3(0.0);
#if BISTRO_LOCAL_LIGHTS == 2
    uvec2 extent = local_light_extent();
    uvec2 pixel = uvec2(clamp(gl_FragCoord.xy, vec2(0.0), vec2(extent - 1u)));
    uvec2 tile_xy = pixel / LOCAL_LIGHT_TILE_SIZE;
    uint tile = min(tile_xy.y * local_light_columns(extent) + tile_xy.x, MAX_LOCAL_LIGHT_TILES - 1u);
    uint offset = tile * LOCAL_LIGHT_TILE_WORDS;
    uint count = min(tile_lists.words[offset], MAX_LOCAL_LIGHTS);
#else
    uint count = light_count;
#endif
    for (uint entry = 0u; entry < count; ++entry) {
#if BISTRO_LOCAL_LIGHTS == 2
        uint index = tile_lists.words[offset + 1u + entry];
#else
        uint index = entry;
#endif
        if (index < light_count) {
            result += point_light_radiance(index, position, n, v, f0, diffuse_color, roughness);
        }
    }
    return result;
}
