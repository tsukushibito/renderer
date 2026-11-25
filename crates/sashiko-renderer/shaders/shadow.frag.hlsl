#define BISTRO_MATERIAL_SHADOW
#ifndef RENDERER_MATERIAL_INCLUDE
#define RENDERER_MATERIAL_INCLUDE "material_sampling.hlsli"
#endif
#include RENDERER_MATERIAL_INCLUDE

void main(
    BISTRO_LOCATION(0) float2 texcoord : TEXCOORD0,
#ifdef BISTRO_TRANSFORMED_DIRECTIONS
    BISTRO_LOCATION(1) nointerpolation uint mirrored : TEXCOORD1,
#endif
    bool front_facing : SV_IsFrontFace
) {
#ifdef BISTRO_TRANSFORMED_DIRECTIONS
    front_facing = front_facing != (mirrored != 0u);
#endif
    sample_shadow_material(texcoord, front_facing);
}
