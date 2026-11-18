const uint LOCAL_LIGHT_TILE_SIZE = 16u;
const uint MAX_LOCAL_LIGHTS = 64u;
const uint MAX_LOCAL_LIGHT_TILES = 65536u;
const uint LOCAL_LIGHT_TILE_WORDS = MAX_LOCAL_LIGHTS + 1u;

struct PointLight {
    vec4 position_radius;
    vec4 color_intensity;
};

layout(set = 0, binding = 2, std430) readonly buffer LocalLights {
    uvec4 extent_count;
    PointLight lights[MAX_LOCAL_LIGHTS];
} local_lights;

uvec2 local_light_extent() {
    return clamp(local_lights.extent_count.xy, uvec2(1u), uvec2(65535u));
}

uint local_light_columns(uvec2 extent) {
    return (extent.x + LOCAL_LIGHT_TILE_SIZE - 1u) / LOCAL_LIGHT_TILE_SIZE;
}

uint local_light_count() {
    return min(local_lights.extent_count.z, MAX_LOCAL_LIGHTS);
}
