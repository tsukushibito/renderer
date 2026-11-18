cbuffer _91_93 : register(b0, space0)
{
    row_major float4x4 _93_m0 : packoffset(c0);
    float4 _93_m1 : packoffset(c4);
    float4 _93_m2 : packoffset(c5);
    float4 _93_m3 : packoffset(c6);
    float4 _93_m4 : packoffset(c7);
    row_major float4x4 _93_m5[4] : packoffset(c8);
    float4 _93_m6 : packoffset(c24);
    float4 _93_m7 : packoffset(c25);
    float4 _93_m8 : packoffset(c26);
    float4 _93_m9 : packoffset(c27);
    float4 _93_m10 : packoffset(c28);
};

cbuffer _24_26 : register(b7, space0)
{
    float4 _26_m0 : packoffset(c0);
    float4 _26_m1 : packoffset(c1);
    float4 _26_m2 : packoffset(c2);
    float4 _26_m3 : packoffset(c3);
    uint _26_m4 : packoffset(c4);
};


static float4 gl_Position;
static int gl_InstanceIndex;
static float3 _82;
static float3 _137;
static float3 _139;
static float3 _140;
static float4 _142;
static float4 _144;
static float2 _148;
static float2 _150;

struct SPIRV_Cross_Input
{
    float3 _82 : TEXCOORD0;
    float3 _140 : TEXCOORD1;
    float4 _144 : TEXCOORD2;
    float2 _150 : TEXCOORD3;
    uint gl_InstanceIndex : SV_InstanceID;
};

struct SPIRV_Cross_Output
{
    float3 _137 : TEXCOORD0;
    float3 _139 : TEXCOORD1;
    float4 _142 : TEXCOORD2;
    float2 _148 : TEXCOORD3;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    uint _166 = min((min(uint(gl_InstanceIndex), 23u) + min(_26_m4, 23u)), 23u);
    precise float3 _171 = _82 - _26_m0.xyz;
    precise float3 _174 = _171 * _26_m0.w;
    precise float3 _181 = _26_m2.xyz * float(_166 % 6u);
    precise float3 _188 = _26_m3.xyz * float(_166 / 6u);
    precise float3 _193 = _26_m1.xyz + _181;
    precise float3 _195 = _193 + _188;
    float3 _198 = _195 + _174;
    precise float4 _101 = _93_m0[0] * _198.x;
    precise float4 _108 = _93_m0[1] * _198.y;
    precise float4 _115 = _93_m0[2] * _198.z;
    precise float4 _119 = _101 + _108;
    precise float4 _123 = _119 + _115;
    precise float4 _128 = _123 + _93_m0[3];
    gl_Position = _128;
    _137 = _198;
    _139 = _140;
    _142 = _144;
    _148 = _150;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_InstanceIndex = int(stage_input.gl_InstanceIndex);
    _82 = stage_input._82;
    _140 = stage_input._140;
    _144 = stage_input._144;
    _150 = stage_input._150;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._137 = _137;
    stage_output._139 = _139;
    stage_output._142 = _142;
    stage_output._148 = _148;
    return stage_output;
}
