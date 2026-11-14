static const uint3 gl_WorkGroupSize = uint3(1u, 1u, 1u);

RWByteAddressBuffer _34 : register(u0, space1);
Texture2D<float4> _16 : register(t0, space0);
SamplerState _20 : register(s1, space0);

uint spvPackUnorm4x8(float4 value)
{
    uint4 Packed = uint4(round(saturate(value) * 255.0));
    return Packed.x | (Packed.y << 8) | (Packed.z << 16) | (Packed.w << 24);
}

float4 spvUnpackUnorm4x8(uint value)
{
    uint4 Packed = uint4(value & 0xff, (value >> 8) & 0xff, (value >> 16) & 0xff, value >> 24);
    return float4(Packed) / 255.0;
}

void comp_main()
{
    _34.Store4(0, uint4(spvPackUnorm4x8(_16.SampleLevel(_20, (-0.25f).xx, 0.0f)), spvPackUnorm4x8(_16.SampleLevel(_20, float2(0.60000002384185791015625f, 0.20000000298023223876953125f), 0.0f)), spvPackUnorm4x8(_16.SampleLevel(_20, float2(0.20000000298023223876953125f, 0.60000002384185791015625f), 0.0f)), spvPackUnorm4x8(_16.SampleLevel(_20, 1.25f.xx, 0.0f))));
}

[numthreads(1, 1, 1)]
void main()
{
    comp_main();
}
