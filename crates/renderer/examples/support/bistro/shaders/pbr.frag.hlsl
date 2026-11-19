#include "material_sampling.hlsli"
#include "pbr_lighting.hlsli"

#if defined(BISTRO_LOCAL_LIGHTS)
#include "local_light_pbr.hlsli"
#endif

struct PixelInput {
    BISTRO_LOCATION(0) float3 world_position : TEXCOORD0;
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
#if BISTRO_LOCAL_LIGHTS == 2
    float4 pixel : SV_Position;
#endif
    bool front_facing : SV_IsFrontFace;
};

float4 main(PixelInput input) : SV_Target0 {
    MaterialSample sampled = sample_material(
        input.world_normal, input.world_tangent, input.texcoord, input.front_facing);
    Surface surface = sampled.surface;
    BrdfInputs brdf = make_brdf_inputs(input.world_position, surface);
    float3 color = evaluate_lighting(input.world_position, surface, brdf);
#if defined(BISTRO_LOCAL_LIGHTS)
#if BISTRO_LOCAL_LIGHTS == 2
    float2 pixel_center = input.pixel.xy;
#else
    float2 pixel_center = 0.0;
#endif
    color += local_light_radiance(
        pixel_center, input.world_position, surface.normal, brdf.view, brdf.f0, brdf.diffuse_color, surface.roughness);
#endif
    // The ideal low-roughness peak may exceed binary16's finite range. Preserve
    // finite linear target values; final SDR clamping remains a separate pass.
    return float4(clamp(color, 0.0, 65504.0), sampled.output_alpha);
}
