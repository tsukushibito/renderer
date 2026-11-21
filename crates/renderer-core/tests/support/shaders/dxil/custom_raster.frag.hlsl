cbuffer _22_24 : register(b2, space0)
{
    float4 _24_m0 : packoffset(c0);
};

ByteAddressBuffer _32 : register(t3, space1);
cbuffer _13_15 : register(b7, space0)
{
    float4 _15_m0 : packoffset(c0);
    float4 _15_m1 : packoffset(c1);
};

Texture2D<float4> _39 : register(t4, space2);
SamplerState _43 : register(s5, space2);

static float4 _9;
static float4 _11;

struct SPIRV_Cross_Input
{
    float4 _11 : TEXCOORD0;
};

struct SPIRV_Cross_Output
{
    float4 _9 : SV_Target0;
};

void frag_main()
{
    _9 = (((_11 * _15_m1) * _24_m0) + asfloat(_32.Load4(0))) * _39.SampleLevel(_43, 0.5f.xx, 0.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _11 = stage_input._11;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._9 = _9;
    return stage_output;
}
