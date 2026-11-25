struct Fill { float4 rgba; };
#ifdef __spirv__
[[vk::push_constant]] Fill fill;
#else
ConstantBuffer<Fill> fill : register(b0);
#endif
float4 main() : SV_Target0 { return fill.rgba; }
