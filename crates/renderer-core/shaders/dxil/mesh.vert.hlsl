cbuffer _17_19
{
    row_major float4x4 _19_m0 : packoffset(c0);
    float4 _19_m1 : packoffset(c4);
};


static float4 gl_Position;
static float3 _25;
static float4 _35;
static float4 _37;

struct SPIRV_Cross_Input
{
    float3 _25 : TEXCOORD0;
    float4 _37 : TEXCOORD1;
};

struct SPIRV_Cross_Output
{
    float4 _35 : TEXCOORD0;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    gl_Position = mul(float4(_25, 1.0f), _19_m0);
    _35 = _37 * _19_m1;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _25 = stage_input._25;
    _37 = stage_input._37;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._35 = _35;
    return stage_output;
}
