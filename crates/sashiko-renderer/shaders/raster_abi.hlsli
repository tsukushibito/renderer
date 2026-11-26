#ifndef BISTRO_RASTER_ABI_HLSLI
#define BISTRO_RASTER_ABI_HLSLI

#ifdef __spirv__
#define BISTRO_LOCATION(n) [[vk::location(n)]]
#else
#define BISTRO_LOCATION(n)
#endif

// The CPU stores columns consecutively. They are indexed as rows in HLSL,
// so matrix products in this file's consumers use mul(vector, matrix).
struct CameraData {
    row_major float4x4 view_projection;
    float4 eye;
    float4 light_direction;
    float4 light_color; // linear sun RGB, orthographic view flag
    float4 environment; // strength, inverse-yaw input radians, specular max LOD, positive W enables shadows; abs(W)>=2 encodes local probe mode and scenario blend
    row_major float4x4 shadow_view_projection[4];
    float4 shadow_splits;
    float4 shadow_texel_world;
    float4 shadow_depth_scale;
    float4 camera_forward_blend;
    float4 shadow_bias; // receiver constant/slope/normal offset in world-texel units
};

struct MaterialData {
    float4 base_factor;
    float4 emissive_normal_scale;
    float4 metallic_roughness_occlusion_cutoff;
    uint4 flags;
};

#endif
