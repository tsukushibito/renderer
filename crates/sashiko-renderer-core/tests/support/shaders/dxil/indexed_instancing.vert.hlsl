ByteAddressBuffer _34 : register(t2, space0);
cbuffer _16_18 : register(b7, space0)
{
    uint _18_m0 : packoffset(c0);
};


static float4 gl_Position;
static int gl_InstanceIndex;
static float2 _45;
static float3 _63;

struct SPIRV_Cross_Input
{
    float2 _45 : TEXCOORD0;
    uint gl_InstanceIndex : SV_InstanceID;
};

struct SPIRV_Cross_Output
{
    float3 _63 : TEXCOORD0;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    uint _25 = min((min(uint(gl_InstanceIndex), 1u) + min(_18_m0, 1u)), 1u);
    gl_Position = float4(_45 + float2(asfloat(_34.Load4(_25 * 16 + 0)).x, 0.0f), 0.5f, 1.0f);
    _63 = asfloat(_34.Load4(_25 * 16 + 0)).yzw;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_InstanceIndex = int(stage_input.gl_InstanceIndex);
    _45 = stage_input._45;
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._63 = _63;
    return stage_output;
}
