#ifdef BISTRO_MATERIAL_SHADOW

#include "raster_abi.hlsli"

ConstantBuffer<MaterialData> material : register(b1, space0);
Texture2D<float4> base_image : register(t0, space1);
SamplerState material_sampler : register(s4, space1);

void sample_shadow_material(float2 texcoord, bool front_facing) {
    // BLEND casts no shadow. MASK preserves its authored threshold. Texture
    // derivatives are computed before any divergent material/face discard.
    float2 dx = ddx(texcoord);
    float2 dy = ddy(texcoord);
    if (material.flags.x == 2u || (!front_facing && material.flags.y == 0u)) {
        discard;
    }
    if (material.flags.x == 1u) {
        float alpha = saturate(material.base_factor.a *
            base_image.SampleGrad(material_sampler, texcoord, dx, dy).a);
        if (alpha < material.metallic_roughness_occlusion_cutoff.w) {
            discard;
        }
    }
}

#else

#include "pbr_math.hlsli"
#include "raster_abi.hlsli"
#include "surface.hlsli"

ConstantBuffer<MaterialData> material : register(b1, space0);
Texture2D<float4> base_image : register(t0, space1);
Texture2D<float4> normal_image : register(t1, space1);
Texture2D<float4> orm_image : register(t2, space1);
Texture2D<float4> emissive_image : register(t3, space1);
SamplerState material_sampler : register(s4, space1);

struct MaterialSample {
    Surface surface;
    float output_alpha;
};

MaterialSample sample_material(
    float3 world_normal, float4 world_tangent, float2 texcoord, bool front_facing
) {
    // Form derivatives while every fragment/helper invocation is active. Later
    // culling/alpha discards cannot make the four texture LODs divergent.
    float2 dx = ddx(texcoord);
    float2 dy = ddy(texcoord);
    if (!front_facing && material.flags.y == 0u) { discard; }
    float4 base = material.base_factor * base_image.SampleGrad(material_sampler, texcoord, dx, dy);
    float alpha = clamp(base.a, 0.0, 1.0);
    uint alpha_mode = material.flags.x;
    if ((alpha_mode == 1u && alpha < material.metallic_roughness_occlusion_cutoff.w)
        || (alpha_mode == 2u && alpha <= 0.0)) { discard; }

    MaterialSample result;
    result.surface.base_color = base.rgb;
    result.output_alpha = alpha_mode == 2u ? alpha : 1.0;
    float3 orm = orm_image.SampleGrad(material_sampler, texcoord, dx, dy).rgb;
    result.surface.metallic = clamp(material.metallic_roughness_occlusion_cutoff.x * orm.b, 0.0, 1.0);
    result.surface.roughness = clamp(material.metallic_roughness_occlusion_cutoff.y * orm.g, 0.0, 1.0);
    result.surface.occlusion = lerp(1.0, orm.r, clamp(material.metallic_roughness_occlusion_cutoff.z, 0.0, 1.0));
    float3 n = unit_or(world_normal, float3(0.0, 0.0, 1.0));
    result.surface.geometric_normal = front_facing ? n : -n;
    float3 t = unit_or(world_tangent.xyz - n * dot(n, world_tangent.xyz),
        abs(n.z) < 0.9 ? unit_or(cross(float3(0.0, 0.0, 1.0), n), float3(1.0, 0.0, 0.0))
                      : unit_or(cross(float3(0.0, 1.0, 0.0), n), float3(1.0, 0.0, 0.0)));
    float3 b = cross(n, t) * (world_tangent.w < 0.0 ? -1.0 : 1.0);
    float3 mapped = normal_image.SampleGrad(material_sampler, texcoord, dx, dy).xyz * 2.0 - 1.0;
    mapped.xy *= material.emissive_normal_scale.w;
    n = unit_or(t * mapped.x + b * mapped.y + n * mapped.z, n);
    result.surface.normal = front_facing ? n : -n;
    result.surface.emissive = material.emissive_normal_scale.rgb *
        emissive_image.SampleGrad(material_sampler, texcoord, dx, dy).rgb;
    return result;
}

#endif
