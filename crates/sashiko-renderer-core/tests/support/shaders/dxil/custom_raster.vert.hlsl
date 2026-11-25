cbuffer _20_22 : register(b7, space0)
{
    float4 _22_m0 : packoffset(c0);
    float4 _22_m1 : packoffset(c1);
};


static float4 gl_Position;
static float2 _18;
static float4 _38;
static float4 _40;

struct SPIRV_Cross_Input
{
    float2 _18 : TEXCOORD0;
    float4 _40 : TEXCOORD1;
};

struct SPIRV_Cross_Output
{
    float4 _38 : TEXCOORD0;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    gl_Position = float4(_18 + _22_m0.xy, _22_m0.z, 1.0f);
    _38 = _40;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _18 = stage_input._18;
    _40 = stage_input._40;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._38 = _38;
    return stage_output;
}
