struct _83
{
    float4 _m0;
    float4 _m1;
};

ByteAddressBuffer _88 : register(t2, space0);
cbuffer _359_361 : register(b0, space0)
{
    row_major float4x4 _361_m0 : packoffset(c0);
    float4 _361_m1 : packoffset(c4);
    float4 _361_m2 : packoffset(c5);
    float4 _361_m3 : packoffset(c6);
    float4 _361_m4 : packoffset(c7);
    row_major float4x4 _361_m5[4] : packoffset(c8);
    float4 _361_m6 : packoffset(c24);
    float4 _361_m7 : packoffset(c25);
    float4 _361_m8 : packoffset(c26);
    float4 _361_m9 : packoffset(c27);
    float4 _361_m10 : packoffset(c28);
};

cbuffer _803_805 : register(b1, space0)
{
    float4 _805_m0 : packoffset(c0);
    float4 _805_m1 : packoffset(c1);
    float4 _805_m2 : packoffset(c2);
    uint4 _805_m3 : packoffset(c3);
};

Texture2D<float4> _431 : register(t0, space3);
SamplerState _435 : register(s4, space3);
Texture2D<float4> _447 : register(t1, space3);
Texture2D<float4> _459 : register(t2, space3);
Texture2D<float4> _467 : register(t3, space3);
Texture2D<float4> _818 : register(t0, space1);
SamplerState _820 : register(s4, space1);
Texture2D<float4> _858 : register(t2, space1);
Texture2D<float4> _953 : register(t1, space1);
Texture2D<float4> _1143 : register(t3, space1);
Texture2D<float4> _1154 : register(t0, space2);
SamplerState _1156 : register(s3, space2);
Texture2D<float4> _1176 : register(t1, space2);
Texture2D<float4> _1187 : register(t2, space2);

static bool gl_FrontFacing;
static float3 _491;
static float2 _791;
static float3 _889;
static float4 _907;
static float4 _1280;

struct SPIRV_Cross_Input
{
    float3 _491 : TEXCOORD0;
    float3 _889 : TEXCOORD1;
    float4 _907 : TEXCOORD2;
    float2 _791 : TEXCOORD3;
    bool gl_FrontFacing : SV_IsFrontFace;
};

struct SPIRV_Cross_Output
{
    float4 _1280 : SV_Target0;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

void frag_main()
{
    float2 _793 = ddx(_791);
    float2 _796 = ddy(_791);
    bool _800 = !gl_FrontFacing;
    bool _811;
    if (_800)
    {
        _811 = _805_m3.y == 0u;
    }
    else
    {
        _811 = _800;
    }
    if (_811)
    {
        discard;
    }
    float4 _827 = _805_m0 * _818.SampleGrad(_820, _791, _793, _796);
    float _831 = clamp(_827.w, 0.0f, 1.0f);
    bool _836 = _805_m3.x == 1u;
    bool _844;
    if (_836)
    {
        _844 = _831 < _805_m2.w;
    }
    else
    {
        _844 = _836;
    }
    bool _853;
    if (!_844)
    {
        _853 = (_805_m3.x == 2u) && (_831 <= 0.0f);
    }
    else
    {
        _853 = _844;
    }
    if (_853)
    {
        discard;
    }
    float4 _865 = _858.SampleGrad(_820, _791, _793, _796);
    float _873 = clamp(_805_m2.x * _865.z, 0.0f, 1.0f);
    float _880 = clamp(_805_m2.y * _865.y, 0.0f, 1.0f);
    float _1325 = dot(_889, _889);
    float3 _2471;
    if (_1325 > 9.9999996826552253889678874634872e-21f)
    {
        _2471 = _889 * rsqrt(_1325);
    }
    else
    {
        _2471 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _2472;
    if (gl_FrontFacing)
    {
        _2472 = _2471;
    }
    else
    {
        _2472 = -_2471;
    }
    float3 _916 = _907.xyz - (_2471 * dot(_2471, _907.xyz));
    float3 _2476;
    if (abs(_2471.z) < 0.89999997615814208984375f)
    {
        float3 _926 = cross(float3(0.0f, 0.0f, 1.0f), _2471);
        float _1343 = dot(_926, _926);
        float3 _2475;
        if (_1343 > 9.9999996826552253889678874634872e-21f)
        {
            _2475 = _926 * rsqrt(_1343);
        }
        else
        {
            _2475 = float3(1.0f, 0.0f, 0.0f);
        }
        _2476 = _2475;
    }
    else
    {
        float3 _934 = cross(float3(0.0f, 1.0f, 0.0f), _2471);
        float _1361 = dot(_934, _934);
        float3 _2474;
        if (_1361 > 9.9999996826552253889678874634872e-21f)
        {
            _2474 = _934 * rsqrt(_1361);
        }
        else
        {
            _2474 = float3(1.0f, 0.0f, 0.0f);
        }
        _2476 = _2474;
    }
    float _1379 = dot(_916, _916);
    float3 _2477;
    if (_1379 > 9.9999996826552253889678874634872e-21f)
    {
        _2477 = _916 * rsqrt(_1379);
    }
    else
    {
        _2477 = _2476;
    }
    float3 _965 = (_953.SampleGrad(_820, _791, _793, _796).xyz * 2.0f) - 1.0f.xxx;
    float2 _970 = _965.xy * _805_m1.w;
    float3 _988 = ((_2477 * _970.x) + ((cross(_2471, _2477) * ((_907.w < 0.0f) ? (-1.0f) : 1.0f)) * _970.y)) + (_2471 * _965.z);
    float _1397 = dot(_988, _988);
    float3 _2482;
    if (_1397 > 9.9999996826552253889678874634872e-21f)
    {
        _2482 = _988 * rsqrt(_1397);
    }
    else
    {
        _2482 = _2471;
    }
    float3 _2483;
    if (_800)
    {
        _2483 = -_2482;
    }
    else
    {
        _2483 = _2482;
    }
    float3 _1004 = _361_m1.xyz - _491;
    float _1415 = dot(_1004, _1004);
    float3 _2484;
    if (_1415 > 9.9999996826552253889678874634872e-21f)
    {
        _2484 = _1004 * rsqrt(_1415);
    }
    else
    {
        _2484 = _2483;
    }
    float _1433 = dot(_361_m2.xyz, _361_m2.xyz);
    float3 _2486;
    if (_1433 > 9.9999996826552253889678874634872e-21f)
    {
        _2486 = _361_m2.xyz * rsqrt(_1433);
    }
    else
    {
        _2486 = _2483;
    }
    float3 _1020 = _2484 + _2486;
    float _1451 = dot(_1020, _1020);
    float3 _2488;
    if (_1451 > 9.9999996826552253889678874634872e-21f)
    {
        _2488 = _1020 * rsqrt(_1451);
    }
    else
    {
        _2488 = _2483;
    }
    float _1029 = max(dot(_2483, _2486), 0.0f);
    float _1034 = max(dot(_2483, _2484), 0.0f);
    float _1039 = max(dot(_2483, _2488), 0.0f);
    float _1049 = max(_880 * _880, 0.001000000047497451305389404296875f);
    float _1053 = _1049 * _1049;
    float _1059 = _1053 - 1.0f;
    float _1061 = ((_1039 * _1039) * _1059) + 1.0f;
    float _1074 = 1.0f - _1053;
    float _1092 = sqrt(_1053 + ((_1074 * _1034) * _1034));
    float3 _1104 = _827.xyz;
    float3 _1107 = lerp(0.039999999105930328369140625f.xxx, _1104, _873.xxx);
    float3 _1116 = 1.0f.xxx - _1107;
    float3 _1119 = _1107 + (_1116 * pow(clamp(1.0f - max(dot(_2484, _2488), 0.0f), 0.0f, 1.0f), 5.0f));
    float3 _1125 = _1104 * (1.0f - _873);
    float _1472 = cos(_361_m4.y);
    float _1475 = sin(_361_m4.y);
    float _1484 = (_1472 * _2483.x) - (_1475 * _2483.z);
    float _1495 = (_1475 * _2483.x) + (_1472 * _2483.z);
    float2 _1498 = float3(_1484, _2483.y, _1495).xz;
    float _2490;
    if (dot(_1498, _1498) > 9.9999996826552253889678874634872e-21f)
    {
        _2490 = frac(0.5f + (atan2(_1495, _1484) * 0.15915493667125701904296875f));
    }
    else
    {
        _2490 = 0.5f;
    }
    float3 _1168 = reflect(-_2484, _2483);
    float _1538 = _1168.x;
    float _1542 = _1168.z;
    float _1544 = (_1472 * _1538) - (_1475 * _1542);
    float _1546 = _1168.y;
    float _1555 = (_1475 * _1538) + (_1472 * _1542);
    float2 _1558 = float3(_1544, _1546, _1555).xz;
    float _2492;
    if (dot(_1558, _1558) > 9.9999996826552253889678874634872e-21f)
    {
        _2492 = frac(0.5f + (atan2(_1555, _1544) * 0.15915493667125701904296875f));
    }
    else
    {
        _2492 = 0.5f;
    }
    uint _1192_dummy_parameter;
    float2 _1200 = 0.5f.xx / float2(int2(spvTextureSize(_1187, uint(0), _1192_dummy_parameter)));
    float4 _1211 = _1187.SampleLevel(_1156, clamp(float2(_1034, _880), _1200, 1.0f.xx - _1200), 0.0f);
    float _2546;
    if (_1029 != 0.0f)
    {
        float _2545;
        do
        {
            if (!(_361_m4.w > 0.0f))
            {
                _2545 = 1.0f;
                break;
            }
            float _1620 = dot(_491 - _361_m1.xyz, _361_m9.xyz);
            bool _1624 = _1620 < _361_m1.w;
            bool _1632;
            if (!_1624)
            {
                _1632 = _1620 > _361_m6.w;
            }
            else
            {
                _1632 = _1624;
            }
            if (_1632)
            {
                _2545 = 1.0f;
                break;
            }
            uint _2494;
            if (_1620 <= _361_m6.x)
            {
                _2494 = 0u;
            }
            else
            {
                uint _2493;
                if (_1620 <= _361_m6.y)
                {
                    _2493 = 1u;
                }
                else
                {
                    _2493 = (_1620 <= _361_m6.z) ? 2u : 3u;
                }
                _2494 = _2493;
            }
            float _1740;
            float _1748;
            float _2512;
            do
            {
                uint _1735 = min(_2494, 3u);
                _1740 = 1.0f - max(dot(_2472, _2486), 0.0f);
                _1748 = _361_m10.z;
                float4 _1762 = mul(float4(_491 + (_2472 * ((_361_m7[_1735] * _1748) * _1740)), 1.0f), _361_m5[_1735]);
                float _1764 = _1762.w;
                if (!(_1764 > 9.9999996826552253889678874634872e-21f))
                {
                    _2512 = 1.0f;
                    break;
                }
                float3 _1774 = _1762.xyz / _1764.xxx;
                float2 _1779 = (_1774.xy * 0.5f) + 0.5f.xx;
                bool _1782 = any(bool2(_1779.x < 0.0f.xx.x, _1779.y < 0.0f.xx.y));
                bool _1789;
                if (!_1782)
                {
                    _1789 = any(bool2(_1779.x > 1.0f.xx.x, _1779.y > 1.0f.xx.y));
                }
                else
                {
                    _1789 = _1782;
                }
                bool _1796;
                if (!_1789)
                {
                    _1796 = _1774.z < 0.0f;
                }
                else
                {
                    _1796 = _1789;
                }
                bool _1803;
                if (!_1796)
                {
                    _1803 = _1774.z > 1.0f;
                }
                else
                {
                    _1803 = _1796;
                }
                if (_1803)
                {
                    _2512 = 1.0f;
                    break;
                }
                float _1822 = _1774.z - ((_361_m7[_1735] * _361_m8[_1735]) * (_361_m10.x + (_361_m10.y * _1740)));
                uint _1827_dummy_parameter;
                float2 _1830 = 1.0f.xx / float2(int2(spvTextureSize(_431, uint(0), _1827_dummy_parameter)));
                float _2499;
                _2499 = 0.0f;
                float _2508;
                for (int _2498 = -1; _2498 <= 1; _2499 = _2508, _2498++)
                {
                    _2508 = _2499;
                    float _2594;
                    for (int _2500 = -1; _2500 <= 1; _2508 = _2594, _2500++)
                    {
                        float2 _1849 = _1779 + (float2(float(_2500), float(_2498)) * _1830);
                        bool _1852 = any(bool2(_1849.x < 0.0f.xx.x, _1849.y < 0.0f.xx.y));
                        bool _1859;
                        if (!_1852)
                        {
                            _1859 = any(bool2(_1849.x > 1.0f.xx.x, _1849.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _1859 = _1852;
                        }
                        if (_1859)
                        {
                            _2594 = _2508 + 1.0f;
                        }
                        else
                        {
                            float _2505;
                            do
                            {
                                if (_1735 == 0u)
                                {
                                    _2505 = _431.SampleLevel(_435, _1849, 0.0f).x;
                                    break;
                                }
                                if (_1735 == 1u)
                                {
                                    _2505 = _447.SampleLevel(_435, _1849, 0.0f).x;
                                    break;
                                }
                                if (_1735 == 2u)
                                {
                                    _2505 = _459.SampleLevel(_435, _1849, 0.0f).x;
                                    break;
                                }
                                _2505 = _467.SampleLevel(_435, _1849, 0.0f).x;
                                break;
                            } while(false);
                            _2594 = _2508 + float(_1822 <= _2505);
                        }
                    }
                }
                _2512 = _2499 * 0.111111111938953399658203125f;
                break;
            } while(false);
            float _2513;
            if (_2494 == 0u)
            {
                _2513 = _361_m1.w;
            }
            else
            {
                _2513 = _361_m6[max(_2494, 1u) - 1u];
            }
            float _1683 = max((_361_m6[_2494] - _2513) * clamp(_361_m9.w, 0.0f, 1.0f), 9.9999999747524270787835121154785e-07f);
            float _1691 = clamp((_1620 - (_361_m6[_2494] - _1683)) / _1683, 0.0f, 1.0f);
            float _2544;
            if (_1691 > 0.0f)
            {
                float _2532;
                if (_2494 < 3u)
                {
                    float _2531;
                    do
                    {
                        uint _1949 = min((_2494 + 1u), 3u);
                        float4 _1976 = mul(float4(_491 + (_2472 * ((_361_m7[_1949] * _1748) * _1740)), 1.0f), _361_m5[_1949]);
                        float _1978 = _1976.w;
                        if (!(_1978 > 9.9999996826552253889678874634872e-21f))
                        {
                            _2531 = 1.0f;
                            break;
                        }
                        float3 _1988 = _1976.xyz / _1978.xxx;
                        float2 _1993 = (_1988.xy * 0.5f) + 0.5f.xx;
                        bool _1996 = any(bool2(_1993.x < 0.0f.xx.x, _1993.y < 0.0f.xx.y));
                        bool _2003;
                        if (!_1996)
                        {
                            _2003 = any(bool2(_1993.x > 1.0f.xx.x, _1993.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _2003 = _1996;
                        }
                        bool _2010;
                        if (!_2003)
                        {
                            _2010 = _1988.z < 0.0f;
                        }
                        else
                        {
                            _2010 = _2003;
                        }
                        bool _2017;
                        if (!_2010)
                        {
                            _2017 = _1988.z > 1.0f;
                        }
                        else
                        {
                            _2017 = _2010;
                        }
                        if (_2017)
                        {
                            _2531 = 1.0f;
                            break;
                        }
                        float _2036 = _1988.z - ((_361_m7[_1949] * _361_m8[_1949]) * (_361_m10.x + (_361_m10.y * _1740)));
                        uint _2041_dummy_parameter;
                        float2 _2044 = 1.0f.xx / float2(int2(spvTextureSize(_431, uint(0), _2041_dummy_parameter)));
                        float _2518;
                        _2518 = 0.0f;
                        float _2527;
                        for (int _2517 = -1; _2517 <= 1; _2518 = _2527, _2517++)
                        {
                            _2527 = _2518;
                            float _2600;
                            for (int _2519 = -1; _2519 <= 1; _2527 = _2600, _2519++)
                            {
                                float2 _2063 = _1993 + (float2(float(_2519), float(_2517)) * _2044);
                                bool _2066 = any(bool2(_2063.x < 0.0f.xx.x, _2063.y < 0.0f.xx.y));
                                bool _2073;
                                if (!_2066)
                                {
                                    _2073 = any(bool2(_2063.x > 1.0f.xx.x, _2063.y > 1.0f.xx.y));
                                }
                                else
                                {
                                    _2073 = _2066;
                                }
                                if (_2073)
                                {
                                    _2600 = _2527 + 1.0f;
                                }
                                else
                                {
                                    float _2524;
                                    do
                                    {
                                        if (_1949 == 0u)
                                        {
                                            _2524 = _431.SampleLevel(_435, _2063, 0.0f).x;
                                            break;
                                        }
                                        if (_1949 == 1u)
                                        {
                                            _2524 = _447.SampleLevel(_435, _2063, 0.0f).x;
                                            break;
                                        }
                                        if (_1949 == 2u)
                                        {
                                            _2524 = _459.SampleLevel(_435, _2063, 0.0f).x;
                                            break;
                                        }
                                        _2524 = _467.SampleLevel(_435, _2063, 0.0f).x;
                                        break;
                                    } while(false);
                                    _2600 = _2527 + float(_2036 <= _2524);
                                }
                            }
                        }
                        _2531 = _2518 * 0.111111111938953399658203125f;
                        break;
                    } while(false);
                    _2532 = _2531;
                }
                else
                {
                    _2532 = 1.0f;
                }
                _2544 = lerp(_2512, _2532, _1691);
            }
            else
            {
                _2544 = _2512;
            }
            _2545 = _2544;
            break;
        } while(false);
        _2546 = _2545;
    }
    else
    {
        _2546 = 1.0f;
    }
    float3 _1263 = ((((((((1.0f.xxx - _1119) * _1125) * 0.3183098733425140380859375f.xxx) + ((_1119 * (_1053 / max((3.1415927410125732421875f * _1061) * _1061, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_1034 * sqrt(_1053 + ((_1074 * _1029) * _1029))) + (_1029 * _1092), 9.9999999747524270787835121154785e-07f)))) * _361_m3.xyz) * _1029) * _2546) + (((((_1154.SampleLevel(_1156, float2(_2490, acos(clamp(_2483.y, -1.0f, 1.0f)) * 0.3183098733425140380859375f), 0.0f).xyz * _1125) * 0.3183098733425140380859375f.xxx) * lerp(1.0f, _865.x, clamp(_805_m2.z, 0.0f, 1.0f))) + (_1176.SampleLevel(_1156, float2(_2492, acos(clamp(_1546, -1.0f, 1.0f)) * 0.3183098733425140380859375f), _880 * max(_361_m4.z, 0.0f)).xyz * ((_1107 * _1211.x) + _1211.y.xxx))) * max(_361_m4.x, 0.0f))) + (_805_m1.xyz * _1143.SampleGrad(_820, _791, _793, _796).xyz);
    uint _2189 = min(_88.Load(8), 64u);
    uint _2574;
    float3 _2575;
    _2575 = 0.0f.xxx;
    _2574 = 0u;
    float3 _2601;
    bool _2162;
    for (;;)
    {
        _2162 = _2574 < _2189;
        if (_2162)
        {
            if (_2162)
            {
                float3 _2579;
                do
                {
                    _83 _2226;
                    _2226._m0 = asfloat(_88.Load4(_2574 * 32 + 16));
                    _2226._m1 = asfloat(_88.Load4(_2574 * 32 + 32));
                    float3 _2235 = _2226._m0.xyz - _491;
                    float _2238 = dot(_2235, _2235);
                    bool _2243 = !(_2226._m0.w > 0.0f);
                    bool _2253;
                    if (!_2243)
                    {
                        _2253 = !(_2238 < (_2226._m0.w * _2226._m0.w));
                    }
                    else
                    {
                        _2253 = _2243;
                    }
                    if (_2253)
                    {
                        _2579 = 0.0f.xxx;
                        break;
                    }
                    float3 _2577;
                    if (_2238 > 9.9999996826552253889678874634872e-21f)
                    {
                        _2577 = _2235 * rsqrt(_2238);
                    }
                    else
                    {
                        _2577 = _2483;
                    }
                    float _2262 = max(dot(_2483, _2577), 0.0f);
                    if (!(_2262 > 0.0f))
                    {
                        _2579 = 0.0f.xxx;
                        break;
                    }
                    float3 _2270 = _2484 + _2577;
                    float _2413 = dot(_2270, _2270);
                    float3 _2578;
                    if (_2413 > 9.9999996826552253889678874634872e-21f)
                    {
                        _2578 = _2270 * rsqrt(_2413);
                    }
                    else
                    {
                        _2578 = _2483;
                    }
                    float _2280 = max(dot(_2483, _2578), 0.0f);
                    float _2298 = ((_2280 * _2280) * _1059) + 1.0f;
                    float3 _2342 = _1107 + (_1116 * pow(clamp(1.0f - max(dot(_2484, _2578), 0.0f), 0.0f, 1.0f), 5.0f));
                    float _2358 = _2238 / (_2226._m0.w * _2226._m0.w);
                    float _2363 = max(1.0f - (_2358 * _2358), 0.0f);
                    _2579 = ((((((1.0f.xxx - _2342) * _1125) * 0.3183098733425140380859375f.xxx) + ((_2342 * (_1053 / max((3.1415927410125732421875f * _2298) * _2298, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_1034 * sqrt(_1053 + ((_1074 * _2262) * _2262))) + (_2262 * _1092), 9.9999999747524270787835121154785e-07f)))) * (max(_2226._m1.xyz, 0.0f.xxx) * max(_2226._m1.w, 0.0f))) * _2262) * ((_2363 * _2363) / max(_2238, 0.00999999977648258209228515625f));
                    break;
                } while(false);
                _2601 = _2575 + _2579;
            }
            else
            {
                _2601 = _2575;
            }
            _2575 = _2601;
            _2574++;
            continue;
        }
        else
        {
            break;
        }
    }
    _1280 = float4(clamp(_1263 + _2575, 0.0f.xxx, 65504.0f.xxx), (_805_m3.x == 2u) ? _831 : 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FrontFacing = stage_input.gl_FrontFacing;
    _491 = stage_input._491;
    _791 = stage_input._791;
    _889 = stage_input._889;
    _907 = stage_input._907;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._1280 = _1280;
    return stage_output;
}
