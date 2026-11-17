cbuffer _32_34 : register(b7, space0)
{
    float4 _34_m0 : packoffset(c0);
};

Texture2D<float4> _23 : register(t0, space0);
SamplerState _27 : register(s1, space0);
Texture2D<float4> _46 : register(t2, space0);
SamplerState _48 : register(s3, space0);

static float4 gl_FragCoord;
static float4 _60;

struct SPIRV_Cross_Input
{
    float4 gl_FragCoord : SV_Position;
};

struct SPIRV_Cross_Output
{
    float4 _60 : SV_Target0;
};

void frag_main()
{
    _60 = float4(_23.SampleLevel(_27, (gl_FragCoord.xy * 0.5f) - 0.375f.xx, _34_m0.x).xy, _46.SampleLevel(_48, gl_FragCoord.xy * 0.25f.xx, 0.0f).x, 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._60 = _60;
    return stage_output;
}
