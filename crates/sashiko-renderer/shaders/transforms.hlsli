#ifndef PBR_TRANSFORMS_HLSLI
#define PBR_TRANSFORMS_HLSLI

// Read-only six-row records. The descriptor declares the full initialized
// prefix matching the compiled table length, independently of draw operands.
#ifndef RENDERER_TRANSFORM_RECORDS
#define RENDERER_TRANSFORM_RECORDS 1024
#endif
static const uint LAST_TRANSFORM = RENDERER_TRANSFORM_RECORDS - 1u;
ByteAddressBuffer transform_table : register(t4, space0);

uint transform_address(uint first, uint instance_id) {
    // Guard both terms before adding: inline words and instance counts are
    // also available to callers using the low-level Graph API.
    uint start = min(first, LAST_TRANSFORM);
    return (start + min(instance_id, LAST_TRANSFORM - start)) * 96u;
}

uint transform_is_mirrored(float parity) {
    return parity < 0.0 ? 1u : 0u;
}

struct TransformRows {
    float4 x;
    float4 y;
    float4 z;
};

TransformRows load_transform_rows(uint address) {
    TransformRows result;
    result.x = asfloat(transform_table.Load4(address));
    result.y = asfloat(transform_table.Load4(address + 16u));
    result.z = asfloat(transform_table.Load4(address + 32u));
    return result;
}

float3 transform_linear(TransformRows rows, float3 value) {
    precise float3 x = float3(rows.x.x, rows.y.x, rows.z.x) * value.x;
    precise float3 y = float3(rows.x.y, rows.y.y, rows.z.y) * value.y;
    precise float3 z = float3(rows.x.z, rows.y.z, rows.z.z) * value.z;
    precise float3 xy = x + y;
    precise float3 xyz = xy + z;
    return xyz;
}

float3 transform_position(TransformRows rows, float3 value) {
    precise float3 position = transform_linear(rows, value)
        + float3(rows.x.w, rows.y.w, rows.z.w);
    return position;
}

#endif
