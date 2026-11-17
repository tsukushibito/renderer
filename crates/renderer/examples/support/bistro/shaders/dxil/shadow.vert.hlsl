cbuffer _26_28 : register(b0, space0)
{
    row_major float4x4 _28_m0 : packoffset(c0);
    float4 _28_m1 : packoffset(c4);
    float4 _28_m2 : packoffset(c5);
    float4 _28_m3 : packoffset(c6);
    float4 _28_m4 : packoffset(c7);
    row_major float4x4 _28_m5[4] : packoffset(c8);
    float4 _28_m6 : packoffset(c24);
    float4 _28_m7 : packoffset(c25);
    float4 _28_m8 : packoffset(c26);
    float4 _28_m9 : packoffset(c27);
    float4 _28_m10 : packoffset(c28);
};

cbuffer _9_11 : register(b7, space0)
{
    uint _11_m0 : packoffset(c0);
};


static float4 gl_Position;
static float3 _40;
static float2 _85;
static float2 _87;

struct SPIRV_Cross_Input
{
    float3 _40 : TEXCOORD0;
    float2 _87 : TEXCOORD3;
};

struct SPIRV_Cross_Output
{
    float2 _85 : TEXCOORD0;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    uint _18 = min(_11_m0, 3u);
    precise float4 _45 = _28_m5[_18][0] * _40.x;
    precise float4 _53 = _28_m5[_18][1] * _40.y;
    precise float4 _61 = _28_m5[_18][2] * _40.z;
    precise float4 _65 = _45 + _53;
    precise float4 _69 = _65 + _61;
    precise float4 _75 = _69 + _28_m5[_18][3];
    gl_Position = _75;
    _85 = _87;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    _40 = stage_input._40;
    _87 = stage_input._87;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._85 = _85;
    return stage_output;
}
