#include "raster_abi.hlsli"

ConstantBuffer<MaterialData> material : register(b1, space0);
Texture2D<float4> base_image : register(t0, space1);
SamplerState material_sampler : register(s4, space1);

void main(
    BISTRO_LOCATION(0) float2 texcoord : TEXCOORD0,
    bool front_facing : SV_IsFrontFace
) {
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
