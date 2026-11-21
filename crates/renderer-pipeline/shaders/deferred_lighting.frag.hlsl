#include "octahedral.hlsli"
#include "pbr_lighting.hlsli"
#ifdef BISTRO_LOCAL_LIGHTS
#include "local_light_pbr.hlsli"
#endif

cbuffer Reconstruction : register(b1, space0) {
    row_major float4x4 inverse_view_projection;
};
Texture2D<float4> base_metallic_image : register(t0, space1);
Texture2D<float4> normal_roughness_image : register(t1, space1);
Texture2D<float4> emissive_occlusion_image : register(t2, space1);
Texture2D<float4> depth_image : register(t3, space1);
SamplerState gbuffer_sampler : register(s4, space1);

float4 main(float4 pixel : SV_Position) : SV_Target0 {
    float2 uv = pixel.xy / texture_size(depth_image);
    float depth = depth_image.SampleLevel(gbuffer_sampler, uv, 0.0).r;
    // Depth is cleared to one and opaque/mask geometry uses LESS. The HDR
    // attachment's clear color is already present where geometry was discarded.
    if (!(depth < 1.0)) { discard; }

    // Both APIs report upper-left pixel centers and use the same logical clip
    // space for reconstruction. The inverse matrix stores one column per HLSL row.
    float2 ndc = uv * 2.0 - 1.0;
    precise float4 x = inverse_view_projection[0] * ndc.x;
    precise float4 y = inverse_view_projection[1] * ndc.y;
    precise float4 z = inverse_view_projection[2] * depth;
    precise float4 xy = x + y;
    precise float4 xyz = xy + z;
    precise float4 world = xyz + inverse_view_projection[3];
    if (!(abs(world.w) > 1e-20)) { discard; }
    float3 position = world.xyz / world.w;

    float4 base = base_metallic_image.SampleLevel(gbuffer_sampler, uv, 0.0);
    float4 normal = normal_roughness_image.SampleLevel(gbuffer_sampler, uv, 0.0);
    float4 emitted = emissive_occlusion_image.SampleLevel(gbuffer_sampler, uv, 0.0);
    Surface surface;
    surface.base_color = base.rgb;
    surface.normal = oct_decode(normal.xy);
    surface.geometric_normal = unpack_geometric_normal(normal.w);
    surface.emissive = emitted.rgb;
    surface.metallic = clamp(base.a, 0.0, 1.0);
    surface.roughness = clamp(normal.z, 0.0, 1.0);
    surface.occlusion = clamp(emitted.a, 0.0, 1.0);
    BrdfInputs brdf = make_brdf_inputs(position, surface);
    float3 color = evaluate_lighting(position, surface, brdf);
#ifdef BISTRO_LOCAL_LIGHTS
    color += local_light_radiance(pixel.xy, position, surface.normal, brdf.view, brdf.f0, brdf.diffuse_color, surface.roughness);
#endif
    return float4(clamp(color, 0.0, 65504.0), 1.0);
}
