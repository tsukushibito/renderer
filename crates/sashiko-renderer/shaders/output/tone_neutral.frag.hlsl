// Adapted from Khronos PBR Neutral, commit
// 180b1a7bddec33f73fe41712a2963cc3ad8e5547. Apache-2.0; see licenses/.
// Changes: explicit nonnegative input, explicit exposure and separate sRGB output.
Texture2D<float4> linear_image : register(t0, space0);
SamplerState source_sampler : register(s1, space0);
struct ToneParameters { float exposure; };
#ifdef __spirv__
[[vk::push_constant]] ConstantBuffer<ToneParameters> tone;
#else
ConstantBuffer<ToneParameters> tone : register(b0, space0);
#endif

float2 source_size() {
    uint width, height;
    linear_image.GetDimensions(width, height);
    return float2(width, height);
}
float3 pbr_neutral(float3 color) {
    const float start_compression = 0.8 - 0.04;
    const float desaturation = 0.15;
    float x = min(color.r, min(color.g, color.b));
    float offset = x < 0.08 ? x - 6.25 * x * x : 0.04;
    color -= offset;
    float peak = max(color.r, max(color.g, color.b));
    if (peak < start_compression) { return color; }
    float d = 1.0 - start_compression;
    float new_peak = 1.0 - d * d / (peak + d - start_compression);
    color *= new_peak / max(peak, 1e-20);
    float g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return lerp(color, new_peak.xxx, g);
}
float4 main(float4 position : SV_Position) : SV_Target0 {
    float2 uv = position.xy / source_size();
    float3 hdr = max(linear_image.SampleLevel(source_sampler, uv, 0.0).rgb, 0.0);
    return float4(clamp(pbr_neutral(hdr * tone.exposure), 0.0, 1.0), 1.0);
}
