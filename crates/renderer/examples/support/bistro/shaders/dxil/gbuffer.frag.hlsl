cbuffer _161_163 : register(b1, space0)
{
    float4 _163_m0 : packoffset(c0);
    float4 _163_m1 : packoffset(c1);
    float4 _163_m2 : packoffset(c2);
    uint4 _163_m3 : packoffset(c3);
};

Texture2D<float4> _182 : register(t0, space1);
SamplerState _186 : register(s4, space1);
Texture2D<float4> _227 : register(t2, space1);
Texture2D<float4> _323 : register(t1, space1);
Texture2D<float4> _374 : register(t3, space1);

static bool gl_FrontFacing;
static float2 _147;
static float3 _259;
static float4 _277;
static float4 _385;
static float4 _393;
static float4 _404;

struct SPIRV_Cross_Input
{
    float3 _259 : TEXCOORD1;
    float4 _277 : TEXCOORD2;
    float2 _147 : TEXCOORD3;
    bool gl_FrontFacing : SV_IsFrontFace;
};

struct SPIRV_Cross_Output
{
    float4 _385 : SV_Target0;
    float4 _393 : SV_Target1;
    float4 _404 : SV_Target2;
};

uint spvPackHalf2x16(float2 value)
{
    uint2 Packed = f32tof16(value);
    return Packed.x | (Packed.y << 16);
}

float2 spvUnpackHalf2x16(uint value)
{
    return f16tof32(uint2(value & 0xffff, value >> 16));
}

void frag_main()
{
    float2 _149 = ddx(_147);
    float2 _152 = ddy(_147);
    bool _156 = !gl_FrontFacing;
    bool _170;
    if (_156)
    {
        _170 = _163_m3.y == 0u;
    }
    else
    {
        _170 = _156;
    }
    if (_170)
    {
        discard;
    }
    float4 _194 = _163_m0 * _182.SampleGrad(_186, _147, _149, _152);
    float _199 = clamp(_194.w, 0.0f, 1.0f);
    bool _204 = _163_m3.x == 1u;
    bool _213;
    if (_204)
    {
        _213 = _199 < _163_m2.w;
    }
    else
    {
        _213 = _204;
    }
    bool _222;
    if (!_213)
    {
        _222 = (_163_m3.x == 2u) && (_199 <= 0.0f);
    }
    else
    {
        _222 = _213;
    }
    if (_222)
    {
        discard;
    }
    float4 _234 = _227.SampleGrad(_186, _147, _149, _152);
    float _420 = dot(_259, _259);
    float3 _649;
    if (_420 > 9.9999996826552253889678874634872e-21f)
    {
        _649 = _259 * rsqrt(_420);
    }
    else
    {
        _649 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _650;
    if (gl_FrontFacing)
    {
        _650 = _649;
    }
    else
    {
        _650 = -_649;
    }
    float3 _286 = _277.xyz - (_649 * dot(_649, _277.xyz));
    float3 _654;
    if (abs(_649.z) < 0.89999997615814208984375f)
    {
        float3 _296 = cross(float3(0.0f, 0.0f, 1.0f), _649);
        float _438 = dot(_296, _296);
        float3 _653;
        if (_438 > 9.9999996826552253889678874634872e-21f)
        {
            _653 = _296 * rsqrt(_438);
        }
        else
        {
            _653 = float3(1.0f, 0.0f, 0.0f);
        }
        _654 = _653;
    }
    else
    {
        float3 _304 = cross(float3(0.0f, 1.0f, 0.0f), _649);
        float _456 = dot(_304, _304);
        float3 _652;
        if (_456 > 9.9999996826552253889678874634872e-21f)
        {
            _652 = _304 * rsqrt(_456);
        }
        else
        {
            _652 = float3(1.0f, 0.0f, 0.0f);
        }
        _654 = _652;
    }
    float _474 = dot(_286, _286);
    float3 _655;
    if (_474 > 9.9999996826552253889678874634872e-21f)
    {
        _655 = _286 * rsqrt(_474);
    }
    else
    {
        _655 = _654;
    }
    float3 _335 = (_323.SampleGrad(_186, _147, _149, _152).xyz * 2.0f) - 1.0f.xxx;
    float2 _341 = _335.xy * _163_m1.w;
    float3 _359 = ((_655 * _341.x) + ((cross(_649, _655) * ((_277.w < 0.0f) ? (-1.0f) : 1.0f)) * _341.y)) + (_649 * _335.z);
    float _492 = dot(_359, _359);
    float3 _660;
    if (_492 > 9.9999996826552253889678874634872e-21f)
    {
        _660 = _359 * rsqrt(_492);
    }
    else
    {
        _660 = _649;
    }
    float3 _661;
    if (_156)
    {
        _661 = -_660;
    }
    else
    {
        _661 = _660;
    }
    _385 = float4(_194.xyz, clamp(_163_m2.x * _234.z, 0.0f, 1.0f));
    float2 _526 = _661.xy / max((abs(_661.x) + abs(_661.y)) + abs(_661.z), 9.9999996826552253889678874634872e-21f).xx;
    float2 _662;
    if (_661.z >= 0.0f)
    {
        _662 = _526;
    }
    else
    {
        bool2 _541 = bool2(_526.x >= 0.0f.xx.x, _526.y >= 0.0f.xx.y);
        _662 = (1.0f.xx - abs(_526.yx)) * float2(_541.x ? 1.0f.xx.x : (-1.0f).xx.x, _541.y ? 1.0f.xx.y : (-1.0f).xx.y);
    }
    float2 _603 = _650.xy / max((abs(_650.x) + abs(_650.y)) + abs(_650.z), 9.9999996826552253889678874634872e-21f).xx;
    float2 _663;
    if (_650.z >= 0.0f)
    {
        _663 = _603;
    }
    else
    {
        bool2 _618 = bool2(_603.x >= 0.0f.xx.x, _603.y >= 0.0f.xx.y);
        _663 = (1.0f.xx - abs(_603.yx)) * float2(_618.x ? 1.0f.xx.x : (-1.0f).xx.x, _618.y ? 1.0f.xx.y : (-1.0f).xx.y);
    }
    float2 _559 = (_663 * 0.5f) + 0.5f.xx;
    bool2 _666 = any(isnan(_559)).xx;
    uint2 _571 = uint2(round(clamp(float2(_666.x ? 0.5f.xx.x : _559.x, _666.y ? 0.5f.xx.y : _559.y), 0.0f.xx, 1.0f.xx) * 127.0f));
    _393 = float4(_662, clamp(_163_m2.y * _234.y, 0.0f, 1.0f), spvUnpackHalf2x16((1024u + _571.x) + (_571.y << 7u)).x);
    _404 = float4(min(_163_m1.xyz * _374.SampleGrad(_186, _147, _149, _152).xyz, 65504.0f.xxx), lerp(1.0f, _234.x, clamp(_163_m2.z, 0.0f, 1.0f)));
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FrontFacing = stage_input.gl_FrontFacing;
    _147 = stage_input._147;
    _259 = stage_input._259;
    _277 = stage_input._277;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._385 = _385;
    stage_output._393 = _393;
    stage_output._404 = _404;
    return stage_output;
}
