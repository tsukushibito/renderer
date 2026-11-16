cbuffer _54_56 : register(b1, space0)
{
    float4 _56_m0 : packoffset(c0);
    float4 _56_m1 : packoffset(c1);
    float4 _56_m2 : packoffset(c2);
    uint4 _56_m3 : packoffset(c3);
};

cbuffer _262_264 : register(b0, space0)
{
    row_major float4x4 _264_m0 : packoffset(c0);
    float4 _264_m1 : packoffset(c4);
    float4 _264_m2 : packoffset(c5);
    float4 _264_m3 : packoffset(c6);
    float4 _264_m4 : packoffset(c7);
};

Texture2D<float4> _77 : register(t0, space1);
SamplerState _81 : register(s4, space1);
Texture2D<float4> _126 : register(t2, space1);
Texture2D<float4> _213 : register(t1, space1);
Texture2D<float4> _416 : register(t3, space1);

static bool gl_FrontFacing;
static float2 _39;
static float3 _158;
static float4 _166;
static float3 _268;
static float4 _448;

struct SPIRV_Cross_Input
{
    float3 _268 : TEXCOORD0;
    float3 _158 : TEXCOORD1;
    float4 _166 : TEXCOORD2;
    float2 _39 : TEXCOORD3;
    bool gl_FrontFacing : SV_IsFrontFace;
};

struct SPIRV_Cross_Output
{
    float4 _448 : SV_Target0;
};

void frag_main()
{
    float2 _41 = ddx(_39);
    float2 _44 = ddy(_39);
    bool _48 = !gl_FrontFacing;
    bool _65;
    if (_48)
    {
        _65 = _56_m3.y == 0u;
    }
    else
    {
        _65 = _48;
    }
    if (_65)
    {
        discard;
    }
    float4 _89 = _56_m0 * _77.SampleGrad(_81, _39, _41, _44);
    float _96 = clamp(_89.w, 0.0f, 1.0f);
    bool _102 = _56_m3.x == 1u;
    bool _111;
    if (_102)
    {
        _111 = _96 < _56_m2.w;
    }
    else
    {
        _111 = _102;
    }
    bool _121;
    if (!_111)
    {
        _121 = (_56_m3.x == 2u) && (_96 <= 0.0f);
    }
    else
    {
        _121 = _111;
    }
    if (_121)
    {
        discard;
    }
    float4 _133 = _126.SampleGrad(_81, _39, _41, _44);
    float _141 = clamp(_56_m2.x * _133.z, 0.0f, 1.0f);
    float _148 = clamp(_56_m2.y * _133.y, 0.0f, 1.0f);
    float _468 = dot(_158, _158);
    float3 _618;
    if (_468 > 9.9999996826552253889678874634872e-21f)
    {
        _618 = _158 * rsqrt(_468);
    }
    else
    {
        _618 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _175 = _166.xyz - (_618 * dot(_618, _166.xyz));
    float3 _621;
    if (abs(_618.z) < 0.89999997615814208984375f)
    {
        float3 _185 = cross(float3(0.0f, 0.0f, 1.0f), _618);
        float _486 = dot(_185, _185);
        float3 _620;
        if (_486 > 9.9999996826552253889678874634872e-21f)
        {
            _620 = _185 * rsqrt(_486);
        }
        else
        {
            _620 = float3(1.0f, 0.0f, 0.0f);
        }
        _621 = _620;
    }
    else
    {
        float3 _193 = cross(float3(0.0f, 1.0f, 0.0f), _618);
        float _504 = dot(_193, _193);
        float3 _619;
        if (_504 > 9.9999996826552253889678874634872e-21f)
        {
            _619 = _193 * rsqrt(_504);
        }
        else
        {
            _619 = float3(1.0f, 0.0f, 0.0f);
        }
        _621 = _619;
    }
    float _522 = dot(_175, _175);
    float3 _622;
    if (_522 > 9.9999996826552253889678874634872e-21f)
    {
        _622 = _175 * rsqrt(_522);
    }
    else
    {
        _622 = _621;
    }
    float3 _225 = (_213.SampleGrad(_81, _39, _41, _44).xyz * 2.0f) - 1.0f.xxx;
    float2 _231 = _225.xy * _56_m1.w;
    float3 _249 = ((_622 * _231.x) + ((cross(_618, _622) * ((_166.w < 0.0f) ? (-1.0f) : 1.0f)) * _231.y)) + (_618 * _225.z);
    float _540 = dot(_249, _249);
    float3 _627;
    if (_540 > 9.9999996826552253889678874634872e-21f)
    {
        _627 = _249 * rsqrt(_540);
    }
    else
    {
        _627 = _618;
    }
    float3 _628;
    if (_48)
    {
        _628 = -_627;
    }
    else
    {
        _628 = _627;
    }
    float3 _270 = _264_m1.xyz - _268;
    float _558 = dot(_270, _270);
    float3 _629;
    if (_558 > 9.9999996826552253889678874634872e-21f)
    {
        _629 = _270 * rsqrt(_558);
    }
    else
    {
        _629 = _628;
    }
    float _576 = dot(_264_m2.xyz, _264_m2.xyz);
    float3 _631;
    if (_576 > 9.9999996826552253889678874634872e-21f)
    {
        _631 = _264_m2.xyz * rsqrt(_576);
    }
    else
    {
        _631 = _628;
    }
    float3 _286 = _629 + _631;
    float _594 = dot(_286, _286);
    float3 _633;
    if (_594 > 9.9999996826552253889678874634872e-21f)
    {
        _633 = _286 * rsqrt(_594);
    }
    else
    {
        _633 = _628;
    }
    float _295 = max(dot(_628, _631), 0.0f);
    float _300 = max(dot(_628, _629), 0.0f);
    float _305 = max(dot(_628, _633), 0.0f);
    float _316 = max(_148 * _148, 0.001000000047497451305389404296875f);
    float _320 = _316 * _316;
    float _328 = ((_305 * _305) * (_320 - 1.0f)) + 1.0f;
    float _343 = 1.0f - _320;
    float3 _375 = _89.xyz;
    float3 _378 = lerp(0.039999999105930328369140625f.xxx, _375, _141.xxx);
    float3 _392 = _378 + ((1.0f.xxx - _378) * pow(clamp(1.0f - max(dot(_629, _633), 0.0f), 0.0f, 1.0f), 5.0f));
    float3 _398 = _375 * (1.0f - _141);
    _448 = float4(clamp((((((((1.0f.xxx - _392) * _398) * 0.3183098733425140380859375f.xxx) + ((_392 * (_320 / max((3.1415927410125732421875f * _328) * _328, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_300 * sqrt(_320 + ((_343 * _295) * _295))) + (_295 * sqrt(_320 + ((_343 * _300) * _300))), 9.9999999747524270787835121154785e-07f)))) * _264_m3.xyz) * _295) + ((_264_m4.xyz * _398) * lerp(1.0f, _133.x, clamp(_56_m2.z, 0.0f, 1.0f)))) + (_56_m1.xyz * _416.SampleGrad(_81, _39, _41, _44).xyz), 0.0f.xxx, 65504.0f.xxx), (_56_m3.x == 2u) ? _96 : 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FrontFacing = stage_input.gl_FrontFacing;
    _39 = stage_input._39;
    _158 = stage_input._158;
    _166 = stage_input._166;
    _268 = stage_input._268;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._448 = _448;
    return stage_output;
}
