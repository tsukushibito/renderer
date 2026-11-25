// Encoded Transform records: affine rows followed by inverse-transpose rows.
cbuffer Palette : register(b0) {
    float4 rows[128 * 6];
};
void deform(float3 p, float3 n, float4 t, uint4 indices, float4 weights,
            out float3 position, out float3 normal, out float3 tangent, out float sign_out) {
    float3 b = cross(n, t.xyz) * t.w;
    position = 0; normal = 0; tangent = 0; float3 bitangent = 0;
    [unroll] for (uint lane = 0; lane < 4; ++lane) {
        float w = weights[lane];
        if (w == 0) continue;
        // The program is safe even for arbitrary binding contents.
        uint joint = indices[lane] & 127;
        [unroll] for (uint axis = 0; axis < 3; ++axis) {
            float4 row = rows[6 * joint + axis];
            position[axis] += w * dot(row, float4(p, 1));
            normal[axis] += w * dot(rows[6 * joint + 3 + axis].xyz, n);
            tangent[axis] += w * dot(row.xyz, t.xyz);
            bitangent[axis] += w * dot(row.xyz, b);
        }
    }
    float handed = dot(cross(normal, tangent), bitangent);
    sign_out = handed == 0 ? t.w : sign(handed);
}
