#include "raster_abi.hlsli"

float4 main(BISTRO_LOCATION(0) nointerpolation float4 id_bytes : TEXCOORD0) : SV_Target0 {
    return id_bytes;
}
