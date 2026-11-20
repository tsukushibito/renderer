#ifndef PBR_DIRECTION_SCALE_HLSLI
#define PBR_DIRECTION_SCALE_HLSLI

uint max_direction_magnitude(float3 value) {
    // Inspect the absolute float representation before any arithmetic can
    // flush a subnormal input. Positive finite float words are ordered.
    uint3 magnitude = asuint(value) & 0x7fffffffu;
    return max(max(magnitude.x, magnitude.y), magnitude.z);
}

int extreme_direction_shift(uint magnitude) {
    // Typical basis inputs already fit the shared material normalization.
    if (magnitude == 0u || magnitude >= 0x7f800000u
        || (magnitude >= asuint(1.0e-8) && magnitude <= asuint(1.0e8))) {
        return 0;
    }
    return clamp(127 - int(magnitude >> 23u), -126, 126);
}

float3 shift_direction(float3 value, int shift) {
    float scale = asfloat(uint(shift + 127) << 23u);
    float3 scaled = value * scale;
    if (shift <= 22) {
        return scaled;
    }
    // A subnormal word is a 23-bit integer significand times 2^-149.
    // Combine that implicit scale with the shift before multiplying.
    uint3 bits = asuint(value);
    float subnormal_scale = asfloat(uint(shift - 22) << 23u);
    float3 subnormal = float3(bits & 0x007fffffu) * subnormal_scale;
    uint3 sign = bits & 0x80000000u;
    return select((bits & 0x7f800000u) == 0u,
        asfloat(asuint(subnormal) | sign), scaled);
}

float3 scale_extreme_direction(float3 value) {
    int shift = extreme_direction_shift(max_direction_magnitude(value));
    return shift == 0 ? value : shift_direction(value, shift);
}

#endif
