// Encoded Transform records: affine rows followed by inverse-transpose rows.
cbuffer Palette : register(b0) {
    float4 rows[128 * 6];
};
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
    float3 b = cross(n, t.xyz) * t.w;
    uint2 indices = rest.Load2(src + 48);
    float4 weights = asfloat(rest.Load4(src + 56));
    float3 position = 0, normal = 0, tangent = 0, bitangent = 0;
    [unroll] for (uint lane = 0; lane < 4; ++lane) {
        float w = weights[lane];
        if (w == 0) continue;
        // The program is safe even for arbitrary binding contents.
        uint joint = (indices[lane / 2] >> (16 * (lane % 2))) & 127;
        [unroll] for (uint axis = 0; axis < 3; ++axis) {
            float4 row = rows[6 * joint + axis];
            position[axis] += w * dot(row, float4(p, 1));
            normal[axis] += w * dot(rows[6 * joint + 3 + axis].xyz, n);
            tangent[axis] += w * dot(row.xyz, t.xyz);
            bitangent[axis] += w * dot(row.xyz, b);
        }
    }
    float handed = dot(cross(normal, tangent), bitangent);
    uint dst = invocation.x * 48;
    posed.Store3(dst, asuint(position));
    posed.Store3(dst + 12, asuint(normal));
    posed.Store4(dst + 24, asuint(float4(tangent, handed == 0 ? t.w : sign(handed))));
    posed.Store2(dst + 40, rest.Load2(src + 40));
}
