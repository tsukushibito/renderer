// Controlled indexed-draw benchmark using the same deformation as the compute pass.
#include "raster_abi.hlsli"
#ifdef SKIN_IN_VERTEX
#include "skin.hlsli"
#endif
struct Input {
    BISTRO_LOCATION(0) float3 p : TEXCOORD0;
    BISTRO_LOCATION(1) float3 n : TEXCOORD1;
    BISTRO_LOCATION(2) float4 t : TEXCOORD2;
    BISTRO_LOCATION(3) float2 uv : TEXCOORD3;
#ifdef SKIN_IN_VERTEX
    BISTRO_LOCATION(4) float4 joints : TEXCOORD4;
    BISTRO_LOCATION(5) float4 weights : TEXCOORD5;
#endif
};
struct Output {
    BISTRO_LOCATION(0) nointerpolation float4 color : TEXCOORD0;
    float4 position : SV_Position;
};
Output main(Input v) {
    float3 p = v.p, n = v.n, t = v.t.xyz;
    float sign_out = v.t.w;
#ifdef SKIN_IN_VERTEX
    deform(v.p, v.n, v.t, uint4(v.joints), v.weights, p, n, t, sign_out);
#endif
    Output o;
    o.position = float4(p.x * 0.65, (p.y - 0.9) * 0.8, 0.5 + p.z * 0.2, 1);
#ifndef __spirv__
    o.position.y = -o.position.y;
#endif
    o.color = float4(saturate(0.5 + 0.2 * (normalize(n) + sign_out * normalize(t)) + 0.1 * v.uv.xyx), 1);
    return o;
}
