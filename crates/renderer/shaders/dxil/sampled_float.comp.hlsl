static const uint3 gl_WorkGroupSize = uint3(1u, 1u, 1u);

RWByteAddressBuffer _36 : register(u0, space1);
Texture2D<float4> _17 : register(t0, space0);
SamplerState _21 : register(s1, space0);

void comp_main()
{
    _36.Store4(0, asuint(_17.SampleLevel(_21, (-0.25f).xx, 0.0f)));
    _36.Store4(16, asuint(_17.SampleLevel(_21, float2(0.60000002384185791015625f, 0.20000000298023223876953125f), 0.0f)));
    _36.Store4(32, asuint(_17.SampleLevel(_21, float2(0.20000000298023223876953125f, 0.60000002384185791015625f), 0.0f)));
    _36.Store4(48, asuint(_17.SampleLevel(_21, 1.25f.xx, 0.0f)));
}

[numthreads(1, 1, 1)]
void main()
{
    comp_main();
}
