#include "material_sampling.hlsli"
#include "octahedral.hlsli"

struct PixelInput {
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
#ifdef BISTRO_TRANSFORMED_DIRECTIONS
    BISTRO_LOCATION(4) nointerpolation uint mirrored : TEXCOORD4;
#endif
    bool front_facing : SV_IsFrontFace;
};

struct GBuffer {
    float4 base_metallic : SV_Target0;
    float4 normal_roughness : SV_Target1;
    float4 emissive_occlusion : SV_Target2;
};

GBuffer main(PixelInput input) {
#ifdef BISTRO_TRANSFORMED_DIRECTIONS
    input.front_facing = input.front_facing != (input.mirrored != 0u);
#endif
    Surface surface = sample_material(
        input.world_normal, input.world_tangent, input.texcoord, input.front_facing).surface;
    GBuffer result;
    result.base_metallic = float4(surface.base_color, surface.metallic);
    result.normal_roughness = float4(
        oct_encode(surface.normal), surface.roughness, pack_geometric_normal(surface.geometric_normal));
    result.emissive_occlusion = float4(min(surface.emissive, 65504.0), surface.occlusion);
    return result;
}
