Texture2D<float4> _17 : register(t0, space0);
SamplerState _21 : register(s1, space0);

static float4 gl_FragCoord;
static float4 _47;

struct SPIRV_Cross_Input
{
    float4 gl_FragCoord : SV_Position;
};

struct SPIRV_Cross_Output
{
    float4 _47 : SV_Target0;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

void frag_main()
{
    uint _29_dummy_parameter;
    _47 = float4(clamp(_17.SampleLevel(_21, gl_FragCoord.xy / float2(int2(spvTextureSize(_17, uint(0), _29_dummy_parameter))), 0.0f).xyz, 0.0f.xxx, 1.0f.xxx), 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._47 = _47;
    return stage_output;
}
