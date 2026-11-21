Texture2D<float4> _12 : register(t0, space1);
SamplerState _16 : register(s1, space1);

static float4 _9;
static float2 _22;
static float4 _27;

struct SPIRV_Cross_Input
{
    float4 _27 : TEXCOORD0;
    float2 _22 : TEXCOORD1;
};

struct SPIRV_Cross_Output
{
    float4 _9 : SV_Target0;
};

void frag_main()
{
    _9 = _12.SampleLevel(_16, _22, 0.0f) * _27;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _22 = stage_input._22;
    _27 = stage_input._27;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._9 = _9;
    return stage_output;
}
