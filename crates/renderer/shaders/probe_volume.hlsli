uint baked_debug() {
    return (uint)clamp(floor((camera.shadow_bias.w - 1.0) * 0.5), 0.0, 3.0);
}
float baked_blend() {
    return saturate(camera.shadow_bias.w - 1.0 - 2.0 * (float)baked_debug());
}
// RBKLGI01, RGB irradiance SH in world space. Bound every texture fetch and
// traversal even for graph users supplying an unrelated image or uniform.
float4 probe_load(uint index, uint2 dims) {
    index = min(index, dims.x * dims.y - 1u);
    return irradiance_image.Load(int3(index % dims.x, index / dims.x, 0));
}
uint probe_index(float value, uint maximum) {
    return (uint)(value >= 0.0 ? min(value, (float)maximum) : 0.0);
}
float3 probe_irradiance(float3 position, float3 n, float blend) {
    uint2 dims;
    irradiance_image.GetDimensions(dims.x, dims.y);
    uint maximum = dims.x * dims.y - 1u;
    float4 bounds = probe_load(0, dims);
    float3 f = (position - bounds.xyz) / max(bounds.w, 1e-20);
    if (any(f < 0.0) || any(f > 1.0)) {
        return lerp(probe_load(2, dims).rgb, probe_load(3, dims).rgb, blend);
    }
    uint node_count = probe_index(probe_load(1, dims).x, maximum / 3u);
    uint base = 4u + node_count * 3u;
    uint node = 0u;
    float size = bounds.w;
    [loop] for (uint depth = 0u; depth <= 10u; ++depth) {
        uint at = 4u + node * 3u;
        uint first = probe_index(probe_load(at, dims).x, node_count);
        if (first == 0u) {
            float4 ids0 = probe_load(at + 1u, dims);
            float4 ids1 = probe_load(at + 2u, dims);
            float3 sum = 0.0;
            float weights = 0.0;
            [loop] for (uint i = 0u; i < 8u; ++i) {
                float3 corner = float3(i & 1u, (i >> 1u) & 1u, i >> 2u);
                float3 trilinear = lerp(1.0 - f, f, corner);
                float weight = trilinear.x * trilinear.y * trilinear.z;
                uint id = probe_index(i < 4u ? ids0[i & 3u] : ids1[i & 3u], maximum / 10u);
                uint p = base + 10u * id;
                float3 delta = (f - corner) * size;
                delta -= float3(probe_load(p + 2u, dims).w, probe_load(p + 3u, dims).w,
                                probe_load(p + 4u, dims).w);
                float4 minus = probe_load(p, dims);
                weight *= saturate(minus.w);
                float3 neg = minus.rgb;
                float3 pos = probe_load(p + 1u, dims).rgb;
                float3 depth_to_wall =
                    float3(delta.x >= 0.0 ? pos.x : neg.x, delta.y >= 0.0 ? pos.y : neg.y,
                           delta.z >= 0.0 ? pos.z : neg.z);
                float3 visibility =
                    saturate(1.0 + (depth_to_wall - abs(delta)) / max(size * 0.05, 1e-6));
                weight *= min(visibility.x, min(visibility.y, visibility.z));
                float3 sh = 0.0;
                [unroll] for (uint k = 0u; k < 4u; ++k) {
                    float3 a = probe_load(p + 2u + k, dims).rgb;
                    float3 b = probe_load(p + 6u + k, dims).rgb;
                    sh += lerp(a, b, blend) * float4(1.0, n)[k];
                }
                sum += sh * weight;
                weights += weight;
            }
            if (baked_debug() == 2u)
                return float3(0.08, 0.25 + 0.07 * depth, 0.8);
            if (baked_debug() == 3u)
                return saturate(weights).xxx;
            return max(sum / max(weights, 1e-6), 0.0);
        }
        uint3 side = (uint3)step(0.5, f);
        node = min(first + side.x + 2u * side.y + 4u * side.z, node_count);
        f = saturate(2.0 * f - (float3)side);
        size *= 0.5;
    }
    return 0.0;
}
