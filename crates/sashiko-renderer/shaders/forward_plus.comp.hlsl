#include "local_lights.hlsli"

cbuffer CullCamera : register(b0, space0) {
    // Each HLSL row is one column of the CPU's logical clip transform.
    row_major float4x4 view_projection;
};

#ifdef __spirv__
[[vk::image_format("rgba16f")]]
[[vk::ext_decorate(25)]] // SPIR-V NonReadable, matching the Graph's write-only use.
#endif
RWTexture2D<float4> tile_masks : register(u3, space0);

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

[numthreads(8, 4, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID) {
    // The storage image and Image2d dispatch define the complete write
    // footprint. The light header can only change tile membership.
    uint2 dimensions;
    tile_masks.GetDimensions(dimensions.x, dimensions.y);
    if (any(dispatch_id.xy >= dimensions)) { return; }

    uint2 extent = local_light_extent();
    uint columns = (extent.x + LOCAL_LIGHT_TILE_SIZE - 1u) / LOCAL_LIGHT_TILE_SIZE;
    uint words = max(dimensions.x / columns, 1u);
    uint2 tile_xy = uint2(dispatch_id.x / words, dispatch_id.y);
    uint2 first = tile_xy * LOCAL_LIGHT_TILE_SIZE;
    uint first_light = (dispatch_id.x % words) * 32u;
    uint mask = 0u;
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
        uint end_light = min(first_light + 32u, local_light_count());
        for (uint index = first_light; index < end_light; ++index) {
            float4 sphere = load_local_light(index).position_radius;
            if (sphere.w > 0.0
                && sphere_touches(left, sphere.xyz, sphere.w)
                && sphere_touches(right, sphere.xyz, sphere.w)
                && sphere_touches(top, sphere.xyz, sphere.w)
                && sphere_touches(bottom, sphere.xyz, sphere.w)
                && sphere_touches(z, sphere.xyz, sphere.w)
                && sphere_touches(far_plane, sphere.xyz, sphere.w)) {
                mask |= 1u << (index - first_light);
            }
        }
    }
    // Binary16 represents every integer in [0, 255] exactly.
    tile_masks[dispatch_id.xy] = float4(
        mask & 255u, (mask >> 8u) & 255u, (mask >> 16u) & 255u, mask >> 24u);
}
