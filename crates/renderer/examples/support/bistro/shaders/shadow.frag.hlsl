#define BISTRO_MATERIAL_SHADOW
#ifndef RENDERER_MATERIAL_INCLUDE
#define RENDERER_MATERIAL_INCLUDE "material_sampling.hlsli"
#endif
#include RENDERER_MATERIAL_INCLUDE

void main(
    BISTRO_LOCATION(0) float2 texcoord : TEXCOORD0,
    bool front_facing : SV_IsFrontFace
) {
    sample_shadow_material(texcoord, front_facing);
}
