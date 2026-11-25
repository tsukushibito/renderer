cbuffer _10_12 : register(b7, space0)
{
    float4 _12_m0 : packoffset(c0);
};


static float4 _9;
static float4 _18;

struct SPIRV_Cross_Output
{
    float4 _9 : SV_Target0;
    float4 _18 : SV_Target1;
};

void frag_main()
{
    _9 = _12_m0;
    _18 = _12_m0.zxyw;
}

SPIRV_Cross_Output main()
{
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._9 = _9;
    stage_output._18 = _18;
    return stage_output;
}
