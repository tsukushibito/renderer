Texture2D<float4> _17 : register(t0, space0);
SamplerState _21 : register(s1, space0);

static float4 gl_FragCoord;
static float4 _67;

struct SPIRV_Cross_Input
{
    float4 gl_FragCoord : SV_Position;
};

struct SPIRV_Cross_Output
{
    float4 _67 : SV_Target0;
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
    float3 _45 = clamp(_17.SampleLevel(_21, gl_FragCoord.xy / float2(int2(spvTextureSize(_17, uint(0), _29_dummy_parameter))), 0.0f).xyz, 0.0f.xxx, 1.0f.xxx);
    float3 _49 = _45 * 12.9200000762939453125f;
    float3 _58 = (pow(_45, 0.4166666567325592041015625f.xxx) * 1.05499994754791259765625f) - 0.054999999701976776123046875f.xxx;
    bool3 _64 = bool3(_45.x > 0.003130800090730190277099609375f.xxx.x, _45.y > 0.003130800090730190277099609375f.xxx.y, _45.z > 0.003130800090730190277099609375f.xxx.z);
    _67 = float4(float3(_64.x ? _58.x : _49.x, _64.y ? _58.y : _49.y, _64.z ? _58.z : _49.z), 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._67 = _67;
    return stage_output;
}
