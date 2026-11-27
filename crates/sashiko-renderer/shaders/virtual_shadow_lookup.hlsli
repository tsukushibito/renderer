// Callers supply the current grid, virtual page table and physical D32 atlas.
// An address of zero is unmapped. Physical neighbours can be unrelated pages.
bool map_texel(float2 light_uv, uint level, out uint2 physical) {
    physical = 0;
    float2 texel = virtual_page_texel(light_uv, level, virtual_origin(level));
    if (!all(texel >= 0.0) || !all(texel < 16384.0)) return false;
    uint2 xy = uint2(texel);
    uint2 page = xy >> 7;
    uint entry = virtual_page_entry(level, page);
    if (entry == 0 || entry > 1024) return false;
    uint slot = entry - 1;
    physical = uint2(slot % 32, slot / 32) * 128 + (xy & 127);
    return true;
}

float virtual_visibility(float3 world, float3 normal, float4 eye_distance, float3 axis_x,
                         float3 axis_y, float4 depth_plane, float4 shadow_bias) {
    float distance = length(world - eye_distance.xyz);
    if (!(distance <= eye_distance.w)) return 1.0;
    float2 uv = float2(dot(axis_x, world), dot(axis_y, world));
    float2 normal_uv = float2(dot(axis_x, normal), dot(axis_y, normal));
    float slope = 1.0 - saturate(dot(normal, cross(axis_x, axis_y)));
    float normal_depth = dot(depth_plane.xyz, normal);
    float depth = dot(float4(world, 1.0), depth_plane);
    if (!(depth >= 0.0 && depth <= 1.0)) return 1.0;

    for (uint level = virtual_receiver_level(distance); level < 8; ++level) {
        float pitch = float(1u << level) / 3200.0;
        float offset = pitch * shadow_bias.z * slope;
        float2 center = uv + normal_uv * offset;
        uint2 center_address;
        if (!map_texel(center, level, center_address)) continue;
        float reference = depth + normal_depth * offset;
        float bias_per_metre = shadow_bias.w * (shadow_bias.x + slope * shadow_bias.y);
        float visible = 0.0;
        for (int y = -1; y <= 1; ++y) {
            for (int x = -1; x <= 1; ++x) {
                float2 tap = center + float2(x, y) * pitch;
                float lit = 1.0;
                for (uint parent = level; parent < 8; ++parent) {
                    uint2 address;
                    if (!map_texel(tap, parent, address)) continue;
                    float bias = float(1u << parent) / 3200.0 * bias_per_metre;
                    lit = reference - bias <= virtual_depth(address) ? 1.0 : 0.0;
                    break;
                }
                visible += lit;
            }
        }
        return visible / 9.0;
    }
    return 1.0;
}
