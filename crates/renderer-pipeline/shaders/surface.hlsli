#ifndef BISTRO_SURFACE_HLSLI
#define BISTRO_SURFACE_HLSLI

struct Surface {
    float3 base_color;
    float3 normal;
    float3 geometric_normal;
    float3 emissive;
    float metallic;
    float roughness;
    float occlusion;
};

#endif
