// Pixel centers and rectangle edges are in target pixels, origin top-left.
struct Rectangle {
    float2 origin;
    float2 size;
};
#ifdef __spirv__
[[vk::push_constant]] Rectangle rect;
#else
ConstantBuffer<Rectangle> rect : register(b0);
#endif
Texture2D<float4> source : register(t0);
SamplerState nearest_clamp : register(s1);

float4 main(float4 pixel : SV_Position) : SV_Target0 {
    return source.SampleLevel(nearest_clamp, (pixel.xy - rect.origin) / rect.size, 0);
}
