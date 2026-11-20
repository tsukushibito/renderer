#ifndef BISTRO_LOCAL_LIGHTS_HLSLI
#define BISTRO_LOCAL_LIGHTS_HLSLI

static const uint LOCAL_LIGHT_TILE_SIZE = 16u;
static const uint MAX_LOCAL_LIGHTS = 64u;

struct PointLight {
    float4 position_radius;
    float4 color_intensity;
};

// Both declarations read a uint4 header followed by 64 two-float4 entries.
// DXC lowers TextureBuffer to a fixed-size read-only SSBO for Vulkan. D3D12
// uses a raw buffer so it can bind the same data via the root SRV.
#ifdef __spirv__
struct LocalLightsData {
    uint4 extent_count;
    PointLight lights[MAX_LOCAL_LIGHTS];
};

TextureBuffer<LocalLightsData> local_lights : register(t2, space0);

uint4 local_light_header() {
    return local_lights.extent_count;
}

PointLight load_point_light(uint index) {
    return local_lights.lights[index];
}
#else
ByteAddressBuffer local_lights : register(t2, space0);

uint4 local_light_header() {
    return local_lights.Load4(0);
}

PointLight load_point_light(uint index) {
    uint offset = 16u + index * 32u;
    PointLight light;
    light.position_radius = asfloat(local_lights.Load4(offset));
    light.color_intensity = asfloat(local_lights.Load4(offset + 16u));
    return light;
}
#endif

uint2 local_light_extent() {
    return clamp(local_light_header().xy, 1u, 65535u);
}

uint local_light_count() {
    return min(local_light_header().z, MAX_LOCAL_LIGHTS);
}

#endif
