#include "local_lights.hlsli"

cbuffer CullCamera : register(b0, space0) {
    // Each HLSL row is one column of the CPU's logical clip transform.
    row_major float4x4 view_projection;
};

#ifdef __spirv__
[[vk::ext_decorate(25)]] // SPIR-V NonReadable, matching the Graph's write-only use.
#endif
RWByteAddressBuffer tile_lists : register(u3, space0);

float4 matrix_row(float4x4 matrix, uint row) {
    return float4(matrix[0][row], matrix[1][row], matrix[2][row], matrix[3][row]);
}

bool sphere_touches(float4 plane, float3 center, float radius) {
    float4 terms = plane * float4(center, 1.0);
    float reach = radius * length(plane.xyz);
    // Retain lights at a plane boundary despite normal FP32 evaluation error.
    float tolerance = (dot(abs(terms), 1.0) + reach + 1.0) * 0.00001;
    return !(dot(terms, 1.0) < -reach - tolerance);
}

[numthreads(32, 1, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    // The reviewed 1D dispatch determines the write address. Untrusted frame
    // dimensions can change membership only, not where or how much we write.
    uint tile = dispatch_id.x;
    uint start = tile * LOCAL_LIGHT_TILE_WORDS;
    uint2 extent = local_light_extent();
    uint columns = local_light_columns(extent);
    uint2 tile_xy = uint2(tile % columns, tile / columns);
    uint2 first = tile_xy * LOCAL_LIGHT_TILE_SIZE;
    uint count = 0u;
    if (all(first < extent)) {
        uint2 end = min(first + LOCAL_LIGHT_TILE_SIZE, extent);
        float2 lower = 2.0 * float2(first) / float2(extent) - 1.0;
        float2 upper = 2.0 * float2(end) / float2(extent) - 1.0;
        float4 x = matrix_row(view_projection, 0);
        float4 y = matrix_row(view_projection, 1);
        float4 z = matrix_row(view_projection, 2);
        float4 w = matrix_row(view_projection, 3);
        float4 left = x - lower.x * w;
        float4 right = upper.x * w - x;
        float4 top = y - lower.y * w;
        float4 bottom = upper.y * w - y;
        float4 far_plane = w - z;
        uint lights = local_light_count();
        for (uint index = 0u; index < lights; ++index) {
            float4 sphere = load_point_light(index).position_radius;
            if (sphere.w > 0.0
                && sphere_touches(left, sphere.xyz, sphere.w)
                && sphere_touches(right, sphere.xyz, sphere.w)
                && sphere_touches(top, sphere.xyz, sphere.w)
                && sphere_touches(bottom, sphere.xyz, sphere.w)
                && sphere_touches(z, sphere.xyz, sphere.w)
                && sphere_touches(far_plane, sphere.xyz, sphere.w)) {
                tile_lists.Store((start + 1u + count) * 4u, index);
                ++count;
            }
        }
    }
    for (uint index = count; index < MAX_LOCAL_LIGHTS; ++index) {
        tile_lists.Store((start + 1u + index) * 4u, 0u);
    }
    tile_lists.Store(start * 4u, count);
}
