#include "skin.hlsli"
ByteAddressBuffer rest : register(t1);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWByteAddressBuffer posed : register(u2);

[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint src = invocation.x * 72;
    float3 p = asfloat(rest.Load3(src));
    float3 n = asfloat(rest.Load3(src + 12));
    float4 t = asfloat(rest.Load4(src + 24));
    uint2 indices = rest.Load2(src + 48);
    float4 weights = asfloat(rest.Load4(src + 56));
    float3 position, normal, tangent;
    float sign_out;
    deform(p, n, t, uint4(indices.x, indices.x >> 16, indices.y, indices.y >> 16),
           weights, position, normal, tangent, sign_out);
    uint dst = invocation.x * 48;
    posed.Store3(dst, asuint(position));
    posed.Store3(dst + 12, asuint(normal));
    posed.Store4(dst + 24, asuint(float4(tangent, sign_out)));
    posed.Store2(dst + 40, rest.Load2(src + 40));
}
