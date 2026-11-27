#include "virtual_shadow_math.hlsli"
#define BISTRO_MATERIAL_SHADOW
#include "material_sampling.hlsli"

void main(BISTRO_LOCATION(0) float2 uv : TEXCOORD0,
          BISTRO_LOCATION(1) nointerpolation uint tile : TEXCOORD1,
          BISTRO_LOCATION(2) nointerpolation uint mirrored : TEXCOORD2,
          float4 pixel : SV_Position, bool front_facing : SV_IsFrontFace) {
    // Our positive light Y maps down the atlas, opposite the CSM projection.
    sample_shadow_material(uv, !front_facing != (mirrored != 0));
    if (any(uint2(pixel.xy) - virtual_tile_origin(tile) >= VSM_TILE)) discard;
}
