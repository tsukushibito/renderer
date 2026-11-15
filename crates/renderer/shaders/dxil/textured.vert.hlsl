cbuffer _33_35 : register(b0, space0)
{
    float4 _35_m0 : packoffset(c0);
};

cbuffer _17_19
{
    row_major float4x4 _19_m0 : packoffset(c0);
    float4 _19_m1 : packoffset(c4);
};


static float4 gl_Position;
static float3 _25;
static float4 _42;
static float4 _44;
static float2 _53;

struct SPIRV_Cross_Input
{
    float3 _25 : TEXCOORD0;
    float4 _44 : TEXCOORD1;
};

struct SPIRV_Cross_Output
{
    float4 _42 : TEXCOORD0;
    float2 _53 : TEXCOORD1;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    gl_Position = mul(float4(_25, 1.0f), _19_m0) + _35_m0;
    _42 = _44 * _19_m1;
    _53 = (_25.xy * 0.5f) + 0.5f.xx;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _25 = stage_input._25;
    _44 = stage_input._44;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._42 = _42;
    stage_output._53 = _53;
    return stage_output;
}
