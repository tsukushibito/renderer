Texture2D<float4> linear_image : register(t0, space0);
SamplerState source_sampler : register(s1, space0);

float2 source_size() {
    uint width, height;
    linear_image.GetDimensions(width, height);
    return float2(width, height);
}
float4 main(float4 position : SV_Position) : SV_Target0 {
    // Equal extents, top-left logical pixels on both APIs; no exposure or tone
    // mapping. Sampling uses the selected mip-zero View and nearest clamp.
    float2 uv = position.xy / source_size();
    float3 linear_color = clamp(linear_image.SampleLevel(source_sampler, uv, 0.0).rgb, 0.0, 1.0);
#ifdef RENDERER_LINEAR_OUTPUT
    return float4(linear_color, 1.0);
#else
    float3 encoded = select(linear_color > 0.0031308,
        1.055 * pow(linear_color, 1.0 / 2.4) - 0.055, 12.92 * linear_color);
    return float4(encoded, 1.0);
#endif
}
