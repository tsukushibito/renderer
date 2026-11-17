cbuffer _14_16 : register(b0, space0)
{
    row_major float4x4 _16_m0 : packoffset(c0);
    float4 _16_m1 : packoffset(c4);
    float4 _16_m2 : packoffset(c5);
    float4 _16_m3 : packoffset(c6);
    float4 _16_m4 : packoffset(c7);
    row_major float4x4 _16_m5[4] : packoffset(c8);
    float4 _16_m6 : packoffset(c24);
    float4 _16_m7 : packoffset(c25);
    float4 _16_m8 : packoffset(c26);
    float4 _16_m9 : packoffset(c27);
    float4 _16_m10 : packoffset(c28);
};


static float4 gl_Position;
static float3 _24;
static float3 _68;
static float3 _70;
static float3 _71;
static float4 _73;
static float4 _75;
static float2 _79;
static float2 _81;

struct SPIRV_Cross_Input
{
    float3 _24 : TEXCOORD0;
    float3 _71 : TEXCOORD1;
    float4 _75 : TEXCOORD2;
    float2 _81 : TEXCOORD3;
};

struct SPIRV_Cross_Output
{
    float3 _68 : TEXCOORD0;
    float3 _70 : TEXCOORD1;
    float4 _73 : TEXCOORD2;
    float2 _79 : TEXCOORD3;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    precise float4 _29 = _16_m0[0] * _24.x;
    precise float4 _37 = _16_m0[1] * _24.y;
    precise float4 _45 = _16_m0[2] * _24.z;
    precise float4 _49 = _29 + _37;
    precise float4 _53 = _49 + _45;
    precise float4 _59 = _53 + _16_m0[3];
    gl_Position = _59;
    _68 = _24;
    _70 = _71;
    _73 = _75;
    _79 = _81;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _24 = stage_input._24;
    _71 = stage_input._71;
    _75 = stage_input._75;
    _81 = stage_input._81;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._68 = _68;
    stage_output._70 = _70;
    stage_output._73 = _73;
    stage_output._79 = _79;
    return stage_output;
}
