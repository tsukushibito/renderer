cbuffer _11_13 : register(b0, space0)
{
    row_major float4x4 _13_m0 : packoffset(c0);
    float4 _13_m1 : packoffset(c4);
    float4 _13_m2 : packoffset(c5);
    float4 _13_m3 : packoffset(c6);
    float4 _13_m4 : packoffset(c7);
};


static float4 gl_Position;
static float3 _21;
static float3 _66;
static float3 _68;
static float3 _69;
static float4 _71;
static float4 _73;
static float2 _77;
static float2 _79;

struct SPIRV_Cross_Input
{
    float3 _21 : TEXCOORD0;
    float3 _69 : TEXCOORD1;
    float4 _73 : TEXCOORD2;
    float2 _79 : TEXCOORD3;
};

struct SPIRV_Cross_Output
{
    float3 _66 : TEXCOORD0;
    float3 _68 : TEXCOORD1;
    float4 _71 : TEXCOORD2;
    float2 _77 : TEXCOORD3;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    precise float4 _27 = _13_m0[0] * _21.x;
    precise float4 _35 = _13_m0[1] * _21.y;
    precise float4 _43 = _13_m0[2] * _21.z;
    precise float4 _47 = _27 + _35;
    precise float4 _51 = _47 + _43;
    precise float4 _57 = _51 + _13_m0[3];
    gl_Position = _57;
    _66 = _21;
    _68 = _69;
    _71 = _73;
    _77 = _79;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _21 = stage_input._21;
    _69 = stage_input._69;
    _73 = stage_input._73;
    _79 = stage_input._79;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._66 = _66;
    stage_output._68 = _68;
    stage_output._71 = _71;
    stage_output._77 = _77;
    return stage_output;
}
