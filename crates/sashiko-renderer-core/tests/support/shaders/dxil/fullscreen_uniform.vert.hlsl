cbuffer _44_46 : register(b0, space0)
{
    float4 _46_m0 : packoffset(c0);
};


static float4 gl_Position;
static int gl_VertexIndex;
static float4 _43;

struct SPIRV_Cross_Input
{
    uint gl_VertexIndex : SV_VertexID;
};

struct SPIRV_Cross_Output
{
    float4 _43 : TEXCOORD0;
    float4 gl_Position : SV_Position;
};

void vert_main()
{
    gl_Position = float4((float2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2)) * 2.0f) - 1.0f.xx, 0.0f, 1.0f);
    _43 = _46_m0;
    gl_Position.y = -gl_Position.y;
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_VertexIndex = int(stage_input.gl_VertexIndex);
    vert_main();
    SPIRV_Cross_Output stage_output;
    stage_output.gl_Position = gl_Position;
    stage_output._43 = _43;
    return stage_output;
}
