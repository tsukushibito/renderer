#ifndef SASHIKO_OCTAHEDRAL_HLSLI
#define SASHIKO_OCTAHEDRAL_HLSLI
#include "pbr_math.hlsli"

float2 oct_wrap(float2 components) {
    float2 sign_nonzero = float2(components.x >= 0.0 ? 1.0 : -1.0, components.y >= 0.0 ? 1.0 : -1.0);
    return (1.0 - abs(components.yx)) * sign_nonzero;
}

float2 oct_encode(float3 direction) {
    float sum = abs(direction.x) + abs(direction.y) + abs(direction.z);
    float2 oct = direction.xy / max(sum, 1e-20);
    return direction.z >= 0.0 ? oct : oct_wrap(oct);
}

float3 oct_decode(float2 oct) {
    float3 direction = float3(oct, 1.0 - abs(oct.x) - abs(oct.y));
    if (direction.z < 0.0) {
        direction.xy = oct_wrap(oct);
    }
    return unit_or(direction, float3(0.0, 0.0, 1.0));
}

// Two 7-bit octahedral coordinates occupy the payload of a finite normal
// binary16 value. Decode to an exact FP32 value before writing RGBA16F.
float pack_geometric_normal(float3 direction) {
    float2 encoded = oct_encode(direction) * 0.5 + 0.5;
    if (any(isnan(encoded))) { encoded = 0.5; }
    uint2 quantized = uint2(round(clamp(encoded, 0.0, 1.0) * 127.0));
    return f16tof32(0x0400u + quantized.x + (quantized.y << 7u));
}

float3 unpack_geometric_normal(float encoded) {
    uint packed = (f32tof16(encoded) - 0x0400u) & 0x3fffu;
    float2 oct = float2(packed & 0x7fu, packed >> 7u) * (2.0 / 127.0) - 1.0;
    return oct_decode(oct);
}

#endif
