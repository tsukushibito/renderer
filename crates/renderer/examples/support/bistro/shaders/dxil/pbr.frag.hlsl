cbuffer _64_66 : register(b0, space0)
{
    row_major float4x4 _66_m0 : packoffset(c0);
    float4 _66_m1 : packoffset(c4);
    float4 _66_m2 : packoffset(c5);
    float4 _66_m3 : packoffset(c6);
    float4 _66_m4 : packoffset(c7);
    row_major float4x4 _66_m5[4] : packoffset(c8);
    float4 _66_m6 : packoffset(c24);
    float4 _66_m7 : packoffset(c25);
    float4 _66_m8 : packoffset(c26);
    float4 _66_m9 : packoffset(c27);
    float4 _66_m10 : packoffset(c28);
};

cbuffer _521_523 : register(b1, space0)
{
    float4 _523_m0 : packoffset(c0);
    float4 _523_m1 : packoffset(c1);
    float4 _523_m2 : packoffset(c2);
    uint4 _523_m3 : packoffset(c3);
};

Texture2D<float4> _142 : register(t0, space3);
SamplerState _146 : register(s4, space3);
Texture2D<float4> _159 : register(t1, space3);
Texture2D<float4> _171 : register(t2, space3);
Texture2D<float4> _179 : register(t3, space3);
Texture2D<float4> _536 : register(t0, space1);
SamplerState _538 : register(s4, space1);
Texture2D<float4> _576 : register(t2, space1);
Texture2D<float4> _671 : register(t1, space1);
Texture2D<float4> _865 : register(t3, space1);
Texture2D<float4> _876 : register(t0, space2);
SamplerState _878 : register(s3, space2);
Texture2D<float4> _898 : register(t1, space2);
Texture2D<float4> _909 : register(t2, space2);

static bool gl_FrontFacing;
static float3 _204;
static float2 _508;
static float3 _607;
static float4 _625;
static float4 _981;

struct SPIRV_Cross_Input
{
    float3 _204 : TEXCOORD0;
    float3 _607 : TEXCOORD1;
    float4 _625 : TEXCOORD2;
    float2 _508 : TEXCOORD3;
    bool gl_FrontFacing : SV_IsFrontFace;
};

struct SPIRV_Cross_Output
{
    float4 _981 : SV_Target0;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

void frag_main()
{
    float2 _510 = ddx(_508);
    float2 _513 = ddy(_508);
    bool _517 = !gl_FrontFacing;
    bool _529;
    if (_517)
    {
        _529 = _523_m3.y == 0u;
    }
    else
    {
        _529 = _517;
    }
    if (_529)
    {
        discard;
    }
    float4 _545 = _523_m0 * _536.SampleGrad(_538, _508, _510, _513);
    float _549 = clamp(_545.w, 0.0f, 1.0f);
    bool _554 = _523_m3.x == 1u;
    bool _562;
    if (_554)
    {
        _562 = _549 < _523_m2.w;
    }
    else
    {
        _562 = _554;
    }
    bool _571;
    if (!_562)
    {
        _571 = (_523_m3.x == 2u) && (_549 <= 0.0f);
    }
    else
    {
        _571 = _562;
    }
    if (_571)
    {
        discard;
    }
    float4 _583 = _576.SampleGrad(_538, _508, _510, _513);
    float _591 = clamp(_523_m2.x * _583.z, 0.0f, 1.0f);
    float _598 = clamp(_523_m2.y * _583.y, 0.0f, 1.0f);
    float _1019 = dot(_607, _607);
    float3 _1876;
    if (_1019 > 9.9999996826552253889678874634872e-21f)
    {
        _1876 = _607 * rsqrt(_1019);
    }
    else
    {
        _1876 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _1877;
    if (gl_FrontFacing)
    {
        _1877 = _1876;
    }
    else
    {
        _1877 = -_1876;
    }
    float3 _634 = _625.xyz - (_1876 * dot(_1876, _625.xyz));
    float3 _1881;
    if (abs(_1876.z) < 0.89999997615814208984375f)
    {
        float3 _644 = cross(float3(0.0f, 0.0f, 1.0f), _1876);
        float _1037 = dot(_644, _644);
        float3 _1880;
        if (_1037 > 9.9999996826552253889678874634872e-21f)
        {
            _1880 = _644 * rsqrt(_1037);
        }
        else
        {
            _1880 = float3(1.0f, 0.0f, 0.0f);
        }
        _1881 = _1880;
    }
    else
    {
        float3 _652 = cross(float3(0.0f, 1.0f, 0.0f), _1876);
        float _1055 = dot(_652, _652);
        float3 _1879;
        if (_1055 > 9.9999996826552253889678874634872e-21f)
        {
            _1879 = _652 * rsqrt(_1055);
        }
        else
        {
            _1879 = float3(1.0f, 0.0f, 0.0f);
        }
        _1881 = _1879;
    }
    float _1073 = dot(_634, _634);
    float3 _1882;
    if (_1073 > 9.9999996826552253889678874634872e-21f)
    {
        _1882 = _634 * rsqrt(_1073);
    }
    else
    {
        _1882 = _1881;
    }
    float3 _683 = (_671.SampleGrad(_538, _508, _510, _513).xyz * 2.0f) - 1.0f.xxx;
    float2 _688 = _683.xy * _523_m1.w;
    float3 _706 = ((_1882 * _688.x) + ((cross(_1876, _1882) * ((_625.w < 0.0f) ? (-1.0f) : 1.0f)) * _688.y)) + (_1876 * _683.z);
    float _1091 = dot(_706, _706);
    float3 _1887;
    if (_1091 > 9.9999996826552253889678874634872e-21f)
    {
        _1887 = _706 * rsqrt(_1091);
    }
    else
    {
        _1887 = _1876;
    }
    float3 _1888;
    if (_517)
    {
        _1888 = -_1887;
    }
    else
    {
        _1888 = _1887;
    }
    float3 _722 = _66_m1.xyz - _204;
    float _1109 = dot(_722, _722);
    float3 _1889;
    if (_1109 > 9.9999996826552253889678874634872e-21f)
    {
        _1889 = _722 * rsqrt(_1109);
    }
    else
    {
        _1889 = _1888;
    }
    float _1127 = dot(_66_m2.xyz, _66_m2.xyz);
    float3 _1891;
    if (_1127 > 9.9999996826552253889678874634872e-21f)
    {
        _1891 = _66_m2.xyz * rsqrt(_1127);
    }
    else
    {
        _1891 = _1888;
    }
    float3 _738 = _1889 + _1891;
    float _1145 = dot(_738, _738);
    float3 _1893;
    if (_1145 > 9.9999996826552253889678874634872e-21f)
    {
        _1893 = _738 * rsqrt(_1145);
    }
    else
    {
        _1893 = _1888;
    }
    float _747 = max(dot(_1888, _1891), 0.0f);
    float _752 = max(dot(_1888, _1889), 0.0f);
    float _757 = max(dot(_1888, _1893), 0.0f);
    float _768 = max(_598 * _598, 0.001000000047497451305389404296875f);
    float _772 = _768 * _768;
    float _780 = ((_757 * _757) * (_772 - 1.0f)) + 1.0f;
    float _794 = 1.0f - _772;
    float3 _824 = _545.xyz;
    float3 _827 = lerp(0.039999999105930328369140625f.xxx, _824, _591.xxx);
    float3 _841 = _827 + ((1.0f.xxx - _827) * pow(clamp(1.0f - max(dot(_1889, _1893), 0.0f), 0.0f, 1.0f), 5.0f));
    float3 _847 = _824 * (1.0f - _591);
    float _1166 = cos(_66_m4.y);
    float _1169 = sin(_66_m4.y);
    float _1178 = (_1166 * _1888.x) - (_1169 * _1888.z);
    float _1189 = (_1169 * _1888.x) + (_1166 * _1888.z);
    float2 _1192 = float3(_1178, _1888.y, _1189).xz;
    float _1895;
    if (dot(_1192, _1192) > 9.9999996826552253889678874634872e-21f)
    {
        _1895 = frac(0.5f + (atan2(_1189, _1178) * 0.15915493667125701904296875f));
    }
    else
    {
        _1895 = 0.5f;
    }
    float3 _890 = reflect(-_1889, _1888);
    float _1232 = _890.x;
    float _1236 = _890.z;
    float _1238 = (_1166 * _1232) - (_1169 * _1236);
    float _1240 = _890.y;
    float _1249 = (_1169 * _1232) + (_1166 * _1236);
    float2 _1252 = float3(_1238, _1240, _1249).xz;
    float _1897;
    if (dot(_1252, _1252) > 9.9999996826552253889678874634872e-21f)
    {
        _1897 = frac(0.5f + (atan2(_1249, _1238) * 0.15915493667125701904296875f));
    }
    else
    {
        _1897 = 0.5f;
    }
    uint _914_dummy_parameter;
    float2 _922 = 0.5f.xx / float2(int2(spvTextureSize(_909, uint(0), _914_dummy_parameter)));
    float4 _933 = _909.SampleLevel(_878, clamp(float2(_752, _598), _922, 1.0f.xx - _922), 0.0f);
    float _1950;
    do
    {
        if (!(_66_m4.w > 0.0f))
        {
            _1950 = 1.0f;
            break;
        }
        float _1314 = dot(_204 - _66_m1.xyz, _66_m9.xyz);
        bool _1318 = _1314 < _66_m1.w;
        bool _1326;
        if (!_1318)
        {
            _1326 = _1314 > _66_m6.w;
        }
        else
        {
            _1326 = _1318;
        }
        if (_1326)
        {
            _1950 = 1.0f;
            break;
        }
        uint _1899;
        if (_1314 <= _66_m6.x)
        {
            _1899 = 0u;
        }
        else
        {
            uint _1898;
            if (_1314 <= _66_m6.y)
            {
                _1898 = 1u;
            }
            else
            {
                _1898 = (_1314 <= _66_m6.z) ? 2u : 3u;
            }
            _1899 = _1898;
        }
        float _1434;
        float _1442;
        float _1917;
        do
        {
            uint _1429 = min(_1899, 3u);
            _1434 = 1.0f - max(dot(_1877, _1891), 0.0f);
            _1442 = _66_m10.z;
            float4 _1456 = mul(float4(_204 + (_1877 * ((_66_m7[_1429] * _1442) * _1434)), 1.0f), _66_m5[_1429]);
            float _1458 = _1456.w;
            if (!(_1458 > 9.9999996826552253889678874634872e-21f))
            {
                _1917 = 1.0f;
                break;
            }
            float3 _1468 = _1456.xyz / _1458.xxx;
            float2 _1473 = (_1468.xy * 0.5f) + 0.5f.xx;
            bool _1476 = any(bool2(_1473.x < 0.0f.xx.x, _1473.y < 0.0f.xx.y));
            bool _1483;
            if (!_1476)
            {
                _1483 = any(bool2(_1473.x > 1.0f.xx.x, _1473.y > 1.0f.xx.y));
            }
            else
            {
                _1483 = _1476;
            }
            bool _1490;
            if (!_1483)
            {
                _1490 = _1468.z < 0.0f;
            }
            else
            {
                _1490 = _1483;
            }
            bool _1497;
            if (!_1490)
            {
                _1497 = _1468.z > 1.0f;
            }
            else
            {
                _1497 = _1490;
            }
            if (_1497)
            {
                _1917 = 1.0f;
                break;
            }
            float _1516 = _1468.z - ((_66_m7[_1429] * _66_m8[_1429]) * (_66_m10.x + (_66_m10.y * _1434)));
            uint _1521_dummy_parameter;
            float2 _1524 = 1.0f.xx / float2(int2(spvTextureSize(_142, uint(0), _1521_dummy_parameter)));
            float _1904;
            _1904 = 0.0f;
            float _1913;
            for (int _1903 = -1; _1903 <= 1; _1904 = _1913, _1903++)
            {
                _1913 = _1904;
                float _1956;
                for (int _1905 = -1; _1905 <= 1; _1913 = _1956, _1905++)
                {
                    float2 _1543 = _1473 + (float2(float(_1905), float(_1903)) * _1524);
                    bool _1546 = any(bool2(_1543.x < 0.0f.xx.x, _1543.y < 0.0f.xx.y));
                    bool _1553;
                    if (!_1546)
                    {
                        _1553 = any(bool2(_1543.x > 1.0f.xx.x, _1543.y > 1.0f.xx.y));
                    }
                    else
                    {
                        _1553 = _1546;
                    }
                    if (_1553)
                    {
                        _1956 = _1913 + 1.0f;
                    }
                    else
                    {
                        float _1910;
                        do
                        {
                            if (_1429 == 0u)
                            {
                                _1910 = _142.SampleLevel(_146, _1543, 0.0f).x;
                                break;
                            }
                            if (_1429 == 1u)
                            {
                                _1910 = _159.SampleLevel(_146, _1543, 0.0f).x;
                                break;
                            }
                            if (_1429 == 2u)
                            {
                                _1910 = _171.SampleLevel(_146, _1543, 0.0f).x;
                                break;
                            }
                            _1910 = _179.SampleLevel(_146, _1543, 0.0f).x;
                            break;
                        } while(false);
                        _1956 = _1913 + float(_1516 <= _1910);
                    }
                }
            }
            _1917 = _1904 * 0.111111111938953399658203125f;
            break;
        } while(false);
        float _1918;
        if (_1899 == 0u)
        {
            _1918 = _66_m1.w;
        }
        else
        {
            _1918 = _66_m6[max(_1899, 1u) - 1u];
        }
        float _1377 = max((_66_m6[_1899] - _1918) * clamp(_66_m9.w, 0.0f, 1.0f), 9.9999999747524270787835121154785e-07f);
        float _1385 = clamp((_1314 - (_66_m6[_1899] - _1377)) / _1377, 0.0f, 1.0f);
        float _1949;
        if (_1385 > 0.0f)
        {
            float _1937;
            if (_1899 < 3u)
            {
                float _1936;
                do
                {
                    uint _1643 = min((_1899 + 1u), 3u);
                    float4 _1670 = mul(float4(_204 + (_1877 * ((_66_m7[_1643] * _1442) * _1434)), 1.0f), _66_m5[_1643]);
                    float _1672 = _1670.w;
                    if (!(_1672 > 9.9999996826552253889678874634872e-21f))
                    {
                        _1936 = 1.0f;
                        break;
                    }
                    float3 _1682 = _1670.xyz / _1672.xxx;
                    float2 _1687 = (_1682.xy * 0.5f) + 0.5f.xx;
                    bool _1690 = any(bool2(_1687.x < 0.0f.xx.x, _1687.y < 0.0f.xx.y));
                    bool _1697;
                    if (!_1690)
                    {
                        _1697 = any(bool2(_1687.x > 1.0f.xx.x, _1687.y > 1.0f.xx.y));
                    }
                    else
                    {
                        _1697 = _1690;
                    }
                    bool _1704;
                    if (!_1697)
                    {
                        _1704 = _1682.z < 0.0f;
                    }
                    else
                    {
                        _1704 = _1697;
                    }
                    bool _1711;
                    if (!_1704)
                    {
                        _1711 = _1682.z > 1.0f;
                    }
                    else
                    {
                        _1711 = _1704;
                    }
                    if (_1711)
                    {
                        _1936 = 1.0f;
                        break;
                    }
                    float _1730 = _1682.z - ((_66_m7[_1643] * _66_m8[_1643]) * (_66_m10.x + (_66_m10.y * _1434)));
                    uint _1735_dummy_parameter;
                    float2 _1738 = 1.0f.xx / float2(int2(spvTextureSize(_142, uint(0), _1735_dummy_parameter)));
                    float _1923;
                    _1923 = 0.0f;
                    float _1932;
                    for (int _1922 = -1; _1922 <= 1; _1923 = _1932, _1922++)
                    {
                        _1932 = _1923;
                        float _1962;
                        for (int _1924 = -1; _1924 <= 1; _1932 = _1962, _1924++)
                        {
                            float2 _1757 = _1687 + (float2(float(_1924), float(_1922)) * _1738);
                            bool _1760 = any(bool2(_1757.x < 0.0f.xx.x, _1757.y < 0.0f.xx.y));
                            bool _1767;
                            if (!_1760)
                            {
                                _1767 = any(bool2(_1757.x > 1.0f.xx.x, _1757.y > 1.0f.xx.y));
                            }
                            else
                            {
                                _1767 = _1760;
                            }
                            if (_1767)
                            {
                                _1962 = _1932 + 1.0f;
                            }
                            else
                            {
                                float _1929;
                                do
                                {
                                    if (_1643 == 0u)
                                    {
                                        _1929 = _142.SampleLevel(_146, _1757, 0.0f).x;
                                        break;
                                    }
                                    if (_1643 == 1u)
                                    {
                                        _1929 = _159.SampleLevel(_146, _1757, 0.0f).x;
                                        break;
                                    }
                                    if (_1643 == 2u)
                                    {
                                        _1929 = _171.SampleLevel(_146, _1757, 0.0f).x;
                                        break;
                                    }
                                    _1929 = _179.SampleLevel(_146, _1757, 0.0f).x;
                                    break;
                                } while(false);
                                _1962 = _1932 + float(_1730 <= _1929);
                            }
                        }
                    }
                    _1936 = _1923 * 0.111111111938953399658203125f;
                    break;
                } while(false);
                _1937 = _1936;
            }
            else
            {
                _1937 = 1.0f;
            }
            _1949 = lerp(_1917, _1937, _1385);
        }
        else
        {
            _1949 = _1917;
        }
        _1950 = _1949;
        break;
    } while(false);
    float3 _979 = ((((((((1.0f.xxx - _841) * _847) * 0.3183098733425140380859375f.xxx) + ((_841 * (_772 / max((3.1415927410125732421875f * _780) * _780, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_752 * sqrt(_772 + ((_794 * _747) * _747))) + (_747 * sqrt(_772 + ((_794 * _752) * _752))), 9.9999999747524270787835121154785e-07f)))) * _66_m3.xyz) * _747) * _1950) + (((((_876.SampleLevel(_878, float2(_1895, acos(clamp(_1888.y, -1.0f, 1.0f)) * 0.3183098733425140380859375f), 0.0f).xyz * _847) * 0.3183098733425140380859375f.xxx) * lerp(1.0f, _583.x, clamp(_523_m2.z, 0.0f, 1.0f))) + (_898.SampleLevel(_878, float2(_1897, acos(clamp(_1240, -1.0f, 1.0f)) * 0.3183098733425140380859375f), _598 * max(_66_m4.z, 0.0f)).xyz * ((_827 * _933.x) + _933.y.xxx))) * max(_66_m4.x, 0.0f))) + (_523_m1.xyz * _865.SampleGrad(_538, _508, _510, _513).xyz);
    _981 = float4(clamp(_979, 0.0f.xxx, 65504.0f.xxx), (_523_m3.x == 2u) ? _549 : 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FrontFacing = stage_input.gl_FrontFacing;
    _204 = stage_input._204;
    _508 = stage_input._508;
    _607 = stage_input._607;
    _625 = stage_input._625;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._981 = _981;
    return stage_output;
}
