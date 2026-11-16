cbuffer _17_19 : register(b0, space0)
{
    row_major float4x4 _19_m0 : packoffset(c0);
    float4 _19_m1 : packoffset(c4);
    float4 _19_m2 : packoffset(c5);
    float4 _19_m3 : packoffset(c6);
    float4 _19_m4 : packoffset(c7);
};


static float4 gl_Position;
static float3 _25;
static float3 _36;
static float3 _38;
static float3 _39;
static float4 _41;
static float4 _43;
static float2 _47;
static float2 _49;

struct SPIRV_Cross_Input
{
    float3 _25 : TEXCOORD0;
    float3 _39 : TEXCOORD1;
    float4 _43 : TEXCOORD2;
    float2 _49 : TEXCOORD3;
};

struct SPIRV_Cross_Output
{
    float3 _36 : TEXCOORD0;
    float3 _38 : TEXCOORD1;
    float4 _41 : TEXCOORD2;
    float2 _47 : TEXCOORD3;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    gl_Position = mul(float4(_25, 1.0f), _19_m0);
    _36 = _25;
    _38 = _39;
    _41 = _43;
    _47 = _49;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _25 = stage_input._25;
    _39 = stage_input._39;
    _43 = stage_input._43;
    _49 = stage_input._49;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._36 = _36;
    stage_output._38 = _38;
    stage_output._41 = _41;
    stage_output._47 = _47;
    return stage_output;
}
