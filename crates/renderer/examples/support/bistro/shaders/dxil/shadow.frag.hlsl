cbuffer _21_23 : register(b1, space0)
{
    float4 _23_m0 : packoffset(c0);
    float4 _23_m1 : packoffset(c1);
    float4 _23_m2 : packoffset(c2);
    uint4 _23_m3 : packoffset(c3);
};

Texture2D<float4> _59 : register(t0, space1);
SamplerState _63 : register(s4, space1);

static bool gl_FrontFacing;
static float2 _11;

struct SPIRV_Cross_Input
{
    float2 _11 : TEXCOORD0;
    bool gl_FrontFacing : SV_IsFrontFace;
};

void frag_main()
{
    float2 _13 = ddx(_11);
    float2 _16 = ddy(_11);
    bool _31 = _23_m3.x == 2u;
    bool _46;
    if (!_31)
    {
        bool _38 = !gl_FrontFacing;
        bool _45;
        if (_38)
        {
            _45 = _23_m3.y == 0u;
        }
        else
        {
            _45 = _38;
        }
        _46 = _45;
    }
    else
    {
        _46 = _31;
    }
    if (_46)
    {
        discard;
    }
    bool _78 = _23_m3.x == 1u;
    bool _86;
    if (_78)
    {
        _86 = clamp(_23_m0.w * _59.SampleGrad(_63, _11, _13, _16).w, 0.0f, 1.0f) < _23_m2.w;
    }
    else
    {
        _86 = _78;
    }
    if (_86)
    {
        discard;
    }
}

void main(SPIRV_Cross_Input stage_input)
{
    gl_FrontFacing = stage_input.gl_FrontFacing;
    _11 = stage_input._11;
    frag_main();
}
