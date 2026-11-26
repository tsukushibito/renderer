#ifndef RENDERER_LOCAL_LIGHTS_HLSLI
#define RENDERER_LOCAL_LIGHTS_HLSLI

static const uint LOCAL_LIGHT_TILE_SIZE = 16u;

struct LocalLight {
    float4 position_radius;
    float4 color_intensity;
    float4 half_width_range;
    float3 half_height;
    float shadow_slot;
};

// Four adjacent RGBA32F texels per record. Slot zero is the view header.
// The sampled view supplies physical bounds on both APIs, including when
// a low-level Graph user binds a different initialized image.
Texture2D<float4> local_lights : register(t2, space0);

uint header_component(float value) {
    return value >= 0.0 && value <= 65535.0 ? uint(value) : 0u;
}

// The half-resolution tiled Forward+ resolve shares t2 with the DIS table
// to stay within the portable 16 sampled textures per fragment stage.
void local_light_region(out uint2 size, out uint2 origin) {
    local_lights.GetDimensions(size.x, size.y);
    origin = 0u;
#if defined(SHADOW_HALF_RESOLVE) && BISTRO_LOCAL_LIGHTS == 2
    float2 extent = local_lights.Load(int3(1, 0, 0)).zw;
    uint2 logical = clamp(uint2(header_component(extent.x), header_component(extent.y)), 1u, size);
    origin.y = size.y - logical.y;
    size = logical;
#endif
}

uint4 local_light_header() {
    uint2 size, origin;
    local_light_region(size, origin);
    float4 fields = local_lights.Load(int3(origin, 0));
    return uint4(header_component(fields.x), header_component(fields.y),
                 header_component(fields.z), header_component(fields.w));
}

uint2 local_light_extent() {
    return max(local_light_header().xy, 1u);
}

uint local_light_count() {
    uint2 size, origin;
    local_light_region(size, origin);
    uint slots = (size.x / 4u) * size.y;
    uint4 header = local_light_header();
    return min(header.z | (header.w << 16u), max(slots, 1u) - 1u);
}

LocalLight load_local_light(uint index) {
    uint2 size, origin;
    local_light_region(size, origin);
    uint columns = max(size.x / 4u, 1u);
    uint slot = index + 1u;
    uint2 texel = origin + uint2(4u * (slot % columns), slot / columns);
    LocalLight light;
    light.position_radius = local_lights.Load(int3(texel, 0));
    light.color_intensity = local_lights.Load(int3(texel + uint2(1, 0), 0));
    light.half_width_range = local_lights.Load(int3(texel + uint2(2, 0), 0));
    float4 edge = local_lights.Load(int3(texel + uint2(3, 0), 0));
    light.half_height = edge.xyz;
    light.shadow_slot = edge.w;
    return light;
}

#endif
