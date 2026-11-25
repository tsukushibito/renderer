#ifndef BISTRO_PBR_MATH_HLSLI
#define BISTRO_PBR_MATH_HLSLI

static const float PI = 3.14159265358979323846;

float3 unit_or(float3 value, float3 fallback) {
    float squared = dot(value, value);
    return squared > 1e-20 ? value * rsqrt(squared) : fallback;
}

float3 evaluate_brdf(
    float nl, float nv, float nh, float vh, float3 f0, float3 diffuse_color, float roughness
) {
    // Isotropic GGX with correlated Smith visibility and Schlick Fresnel.
    // The positive alpha floor regularizes the ideal roughness-zero singularity;
    // authored material values remain unchanged in the cooked input/manifest.
    float a = max(roughness * roughness, 1e-3);
    float a2 = a * a;
    float denominator = nh * nh * (a2 - 1.0) + 1.0;
    float distribution = a2 / max(PI * denominator * denominator, 1e-12);
    float visibility_l = nv * sqrt(a2 + (1.0 - a2) * nl * nl);
    float visibility_v = nl * sqrt(a2 + (1.0 - a2) * nv * nv);
    float visibility = 0.5 / max(visibility_l + visibility_v, 1e-6);
    float schlick = pow(clamp(1.0 - vh, 0.0, 1.0), 5.0);
    float3 fresnel = f0 + (1.0 - f0) * schlick;
    float3 diffuse = (1.0 - fresnel) * diffuse_color / PI;
    float3 specular = fresnel * distribution * visibility;
    return diffuse + specular;
}

float2 texture_size(Texture2D<float4> image) {
    uint width;
    uint height;
    uint levels;
    image.GetDimensions(0, width, height, levels);
    return float2(width, height);
}

#endif
