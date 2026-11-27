cbuffer Camera : register(b0) {
    row_major float4x4 world_from_clip;
    float4 eye_distance;
    float4 light_x;
    float4 light_y;
    int4 page_origin[8];
    float4 depth_plane;
    float4 shadow_bias; // constant, slope, normal offset (texels), depth per metre
};

float2 page_texel(float2 light_uv, uint level) {
    float texels_per_metre = 3200.0 / float(1u << level);
    return light_uv * texels_per_metre - float2(page_origin[level].xy) * 128.0;
}

float4 receiver_world(uint2 pixel, uint2 extent, float depth) {
    float2 ndc = (float2(pixel) + 0.5) / float2(extent) * 2.0 - 1.0;
    return world_from_clip[0] * ndc.x + world_from_clip[1] * ndc.y
        + world_from_clip[2] * depth + world_from_clip[3];
}

uint receiver_level(float distance) {
    uint level = 0;
    while (level < 7 && distance >= 1.28 * float(1u << level)) ++level;
    return level;
}
