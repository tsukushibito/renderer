// Example material. Quantize interpolated vertex shading into four bands.
float4 main(float4 shaded : TEXCOORD0) : SV_Target0 {
    return float4(floor(saturate(shaded.rgb) * 3.0 + 0.5) / 3.0, shaded.a);
}
