// Resolve the biased centre once at the finest resident level. Filter taps
// address this tile's rasterized guard band, including at a clipmap corner.
bool virtual_address(float2 light_uv, uint level, out float2 atlas_texel) {
    atlas_texel = 0;
    float2 texel = virtual_page_texel(light_uv, level, virtual_origin(level));
    if (!all(texel >= 0.0) || !all(texel < 16384.0)) return false;
    uint2 page = uint2(texel) >> 7;
    uint entry = virtual_page_entry(level, page);
    if (entry == 0 || entry > VSM_CAPACITY) return false;
    atlas_texel = float2(virtual_tile_origin(entry - 1) + VSM_GUTTER)
                + (texel - float2(page * 128));
    return true;
}

float virtual_visibility(float3 world, float3 normal, float4 eye_distance, float4 axis_x,
                         float3 axis_y, float4 depth_plane, float4 shadow_bias) {
    float distance = length(world - eye_distance.xyz);
    if (!(distance <= eye_distance.w)) return 1.0;
    float2 uv = float2(dot(axis_x.xyz, world), dot(axis_y, world));
    float2 normal_uv = float2(dot(axis_x.xyz, normal), dot(axis_y, normal));
    float slope = 1.0 - saturate(dot(normal, cross(axis_x.xyz, axis_y)));
    float normal_depth = dot(depth_plane.xyz, normal);
    float depth = dot(float4(world, 1.0), depth_plane);
    if (!(depth >= 0.0 && depth <= 1.0)) return 1.0;

    for (uint level = virtual_receiver_level(distance * axis_x.w); level < 8; ++level) {
        float pitch = float(1u << level) / 3200.0;
        float offset = pitch * shadow_bias.z * slope;
        float2 center = uv + normal_uv * offset;
        float2 center_address;
        if (!virtual_address(center, level, center_address)) continue;
        float bias = pitch * shadow_bias.w * (shadow_bias.x + slope * shadow_bias.y);
        float reference = depth + normal_depth * offset - bias;
        float visible = 0.0;
        [unroll]
        for (int y = -1; y <= 1; ++y) {
            [unroll]
            for (int x = -1; x <= 1; ++x) {
                visible += virtual_compare((center_address + float2(x, y)) / 4096.0, reference);
            }
        }
        return visible / 9.0;
    }
    return 1.0;
}
