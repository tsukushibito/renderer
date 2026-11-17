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
static float4 _987;

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
    float4 _987 : SV_Target0;
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
    float _1025 = dot(_607, _607);
    float3 _1882;
    if (_1025 > 9.9999996826552253889678874634872e-21f)
    {
        _1882 = _607 * rsqrt(_1025);
    }
    else
    {
        _1882 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _1883;
    if (gl_FrontFacing)
    {
        _1883 = _1882;
    }
    else
    {
        _1883 = -_1882;
    }
    float3 _634 = _625.xyz - (_1882 * dot(_1882, _625.xyz));
    float3 _1887;
    if (abs(_1882.z) < 0.89999997615814208984375f)
    {
        float3 _644 = cross(float3(0.0f, 0.0f, 1.0f), _1882);
        float _1043 = dot(_644, _644);
        float3 _1886;
        if (_1043 > 9.9999996826552253889678874634872e-21f)
        {
            _1886 = _644 * rsqrt(_1043);
        }
        else
        {
            _1886 = float3(1.0f, 0.0f, 0.0f);
        }
        _1887 = _1886;
    }
    else
    {
        float3 _652 = cross(float3(0.0f, 1.0f, 0.0f), _1882);
        float _1061 = dot(_652, _652);
        float3 _1885;
        if (_1061 > 9.9999996826552253889678874634872e-21f)
        {
            _1885 = _652 * rsqrt(_1061);
        }
        else
        {
            _1885 = float3(1.0f, 0.0f, 0.0f);
        }
        _1887 = _1885;
    }
    float _1079 = dot(_634, _634);
    float3 _1888;
    if (_1079 > 9.9999996826552253889678874634872e-21f)
    {
        _1888 = _634 * rsqrt(_1079);
    }
    else
    {
        _1888 = _1887;
    }
    float3 _683 = (_671.SampleGrad(_538, _508, _510, _513).xyz * 2.0f) - 1.0f.xxx;
    float2 _688 = _683.xy * _523_m1.w;
    float3 _706 = ((_1888 * _688.x) + ((cross(_1882, _1888) * ((_625.w < 0.0f) ? (-1.0f) : 1.0f)) * _688.y)) + (_1882 * _683.z);
    float _1097 = dot(_706, _706);
    float3 _1893;
    if (_1097 > 9.9999996826552253889678874634872e-21f)
    {
        _1893 = _706 * rsqrt(_1097);
    }
    else
    {
        _1893 = _1882;
    }
    float3 _1894;
    if (_517)
    {
        _1894 = -_1893;
    }
    else
    {
        _1894 = _1893;
    }
    float3 _722 = _66_m1.xyz - _204;
    float _1115 = dot(_722, _722);
    float3 _1895;
    if (_1115 > 9.9999996826552253889678874634872e-21f)
    {
        _1895 = _722 * rsqrt(_1115);
    }
    else
    {
        _1895 = _1894;
    }
    float _1133 = dot(_66_m2.xyz, _66_m2.xyz);
    float3 _1897;
    if (_1133 > 9.9999996826552253889678874634872e-21f)
    {
        _1897 = _66_m2.xyz * rsqrt(_1133);
    }
    else
    {
        _1897 = _1894;
    }
    float3 _738 = _1895 + _1897;
    float _1151 = dot(_738, _738);
    float3 _1899;
    if (_1151 > 9.9999996826552253889678874634872e-21f)
    {
        _1899 = _738 * rsqrt(_1151);
    }
    else
    {
        _1899 = _1894;
    }
    float _747 = max(dot(_1894, _1897), 0.0f);
    float _752 = max(dot(_1894, _1895), 0.0f);
    float _757 = max(dot(_1894, _1899), 0.0f);
    float _768 = max(_598 * _598, 0.001000000047497451305389404296875f);
    float _772 = _768 * _768;
    float _780 = ((_757 * _757) * (_772 - 1.0f)) + 1.0f;
    float _794 = 1.0f - _772;
    float3 _824 = _545.xyz;
    float3 _827 = lerp(0.039999999105930328369140625f.xxx, _824, _591.xxx);
    float3 _841 = _827 + ((1.0f.xxx - _827) * pow(clamp(1.0f - max(dot(_1895, _1899), 0.0f), 0.0f, 1.0f), 5.0f));
    float3 _847 = _824 * (1.0f - _591);
    float _1172 = cos(_66_m4.y);
    float _1175 = sin(_66_m4.y);
    float _1184 = (_1172 * _1894.x) - (_1175 * _1894.z);
    float _1195 = (_1175 * _1894.x) + (_1172 * _1894.z);
    float2 _1198 = float3(_1184, _1894.y, _1195).xz;
    float _1901;
    if (dot(_1198, _1198) > 9.9999996826552253889678874634872e-21f)
    {
        _1901 = frac(0.5f + (atan2(_1195, _1184) * 0.15915493667125701904296875f));
    }
    else
    {
        _1901 = 0.5f;
    }
    float3 _890 = reflect(-_1895, _1894);
    float _1238 = _890.x;
    float _1242 = _890.z;
    float _1244 = (_1172 * _1238) - (_1175 * _1242);
    float _1246 = _890.y;
    float _1255 = (_1175 * _1238) + (_1172 * _1242);
    float2 _1258 = float3(_1244, _1246, _1255).xz;
    float _1903;
    if (dot(_1258, _1258) > 9.9999996826552253889678874634872e-21f)
    {
        _1903 = frac(0.5f + (atan2(_1255, _1244) * 0.15915493667125701904296875f));
    }
    else
    {
        _1903 = 0.5f;
    }
    uint _914_dummy_parameter;
    float2 _922 = 0.5f.xx / float2(int2(spvTextureSize(_909, uint(0), _914_dummy_parameter)));
    float4 _933 = _909.SampleLevel(_878, clamp(float2(_752, _598), _922, 1.0f.xx - _922), 0.0f);
    float _1957;
    if (_747 != 0.0f)
    {
        float _1956;
        do
        {
            if (!(_66_m4.w > 0.0f))
            {
                _1956 = 1.0f;
                break;
            }
            float _1320 = dot(_204 - _66_m1.xyz, _66_m9.xyz);
            bool _1324 = _1320 < _66_m1.w;
            bool _1332;
            if (!_1324)
            {
                _1332 = _1320 > _66_m6.w;
            }
            else
            {
                _1332 = _1324;
            }
            if (_1332)
            {
                _1956 = 1.0f;
                break;
            }
            uint _1905;
            if (_1320 <= _66_m6.x)
            {
                _1905 = 0u;
            }
            else
            {
                uint _1904;
                if (_1320 <= _66_m6.y)
                {
                    _1904 = 1u;
                }
                else
                {
                    _1904 = (_1320 <= _66_m6.z) ? 2u : 3u;
                }
                _1905 = _1904;
            }
            float _1440;
            float _1448;
            float _1923;
            do
            {
                uint _1435 = min(_1905, 3u);
                _1440 = 1.0f - max(dot(_1883, _1897), 0.0f);
                _1448 = _66_m10.z;
                float4 _1462 = mul(float4(_204 + (_1883 * ((_66_m7[_1435] * _1448) * _1440)), 1.0f), _66_m5[_1435]);
                float _1464 = _1462.w;
                if (!(_1464 > 9.9999996826552253889678874634872e-21f))
                {
                    _1923 = 1.0f;
                    break;
                }
                float3 _1474 = _1462.xyz / _1464.xxx;
                float2 _1479 = (_1474.xy * 0.5f) + 0.5f.xx;
                bool _1482 = any(bool2(_1479.x < 0.0f.xx.x, _1479.y < 0.0f.xx.y));
                bool _1489;
                if (!_1482)
                {
                    _1489 = any(bool2(_1479.x > 1.0f.xx.x, _1479.y > 1.0f.xx.y));
                }
                else
                {
                    _1489 = _1482;
                }
                bool _1496;
                if (!_1489)
                {
                    _1496 = _1474.z < 0.0f;
                }
                else
                {
                    _1496 = _1489;
                }
                bool _1503;
                if (!_1496)
                {
                    _1503 = _1474.z > 1.0f;
                }
                else
                {
                    _1503 = _1496;
                }
                if (_1503)
                {
                    _1923 = 1.0f;
                    break;
                }
                float _1522 = _1474.z - ((_66_m7[_1435] * _66_m8[_1435]) * (_66_m10.x + (_66_m10.y * _1440)));
                uint _1527_dummy_parameter;
                float2 _1530 = 1.0f.xx / float2(int2(spvTextureSize(_142, uint(0), _1527_dummy_parameter)));
                float _1910;
                _1910 = 0.0f;
                float _1919;
                for (int _1909 = -1; _1909 <= 1; _1910 = _1919, _1909++)
                {
                    _1919 = _1910;
                    float _1963;
                    for (int _1911 = -1; _1911 <= 1; _1919 = _1963, _1911++)
                    {
                        float2 _1549 = _1479 + (float2(float(_1911), float(_1909)) * _1530);
                        bool _1552 = any(bool2(_1549.x < 0.0f.xx.x, _1549.y < 0.0f.xx.y));
                        bool _1559;
                        if (!_1552)
                        {
                            _1559 = any(bool2(_1549.x > 1.0f.xx.x, _1549.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _1559 = _1552;
                        }
                        if (_1559)
                        {
                            _1963 = _1919 + 1.0f;
                        }
                        else
                        {
                            float _1916;
                            do
                            {
                                if (_1435 == 0u)
                                {
                                    _1916 = _142.SampleLevel(_146, _1549, 0.0f).x;
                                    break;
                                }
                                if (_1435 == 1u)
                                {
                                    _1916 = _159.SampleLevel(_146, _1549, 0.0f).x;
                                    break;
                                }
                                if (_1435 == 2u)
                                {
                                    _1916 = _171.SampleLevel(_146, _1549, 0.0f).x;
                                    break;
                                }
                                _1916 = _179.SampleLevel(_146, _1549, 0.0f).x;
                                break;
                            } while(false);
                            _1963 = _1919 + float(_1522 <= _1916);
                        }
                    }
                }
                _1923 = _1910 * 0.111111111938953399658203125f;
                break;
            } while(false);
            float _1924;
            if (_1905 == 0u)
            {
                _1924 = _66_m1.w;
            }
            else
            {
                _1924 = _66_m6[max(_1905, 1u) - 1u];
            }
            float _1383 = max((_66_m6[_1905] - _1924) * clamp(_66_m9.w, 0.0f, 1.0f), 9.9999999747524270787835121154785e-07f);
            float _1391 = clamp((_1320 - (_66_m6[_1905] - _1383)) / _1383, 0.0f, 1.0f);
            float _1955;
            if (_1391 > 0.0f)
            {
                float _1943;
                if (_1905 < 3u)
                {
                    float _1942;
                    do
                    {
                        uint _1649 = min((_1905 + 1u), 3u);
                        float4 _1676 = mul(float4(_204 + (_1883 * ((_66_m7[_1649] * _1448) * _1440)), 1.0f), _66_m5[_1649]);
                        float _1678 = _1676.w;
                        if (!(_1678 > 9.9999996826552253889678874634872e-21f))
                        {
                            _1942 = 1.0f;
                            break;
                        }
                        float3 _1688 = _1676.xyz / _1678.xxx;
                        float2 _1693 = (_1688.xy * 0.5f) + 0.5f.xx;
                        bool _1696 = any(bool2(_1693.x < 0.0f.xx.x, _1693.y < 0.0f.xx.y));
                        bool _1703;
                        if (!_1696)
                        {
                            _1703 = any(bool2(_1693.x > 1.0f.xx.x, _1693.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _1703 = _1696;
                        }
                        bool _1710;
                        if (!_1703)
                        {
                            _1710 = _1688.z < 0.0f;
                        }
                        else
                        {
                            _1710 = _1703;
                        }
                        bool _1717;
                        if (!_1710)
                        {
                            _1717 = _1688.z > 1.0f;
                        }
                        else
                        {
                            _1717 = _1710;
                        }
                        if (_1717)
                        {
                            _1942 = 1.0f;
                            break;
                        }
                        float _1736 = _1688.z - ((_66_m7[_1649] * _66_m8[_1649]) * (_66_m10.x + (_66_m10.y * _1440)));
                        uint _1741_dummy_parameter;
                        float2 _1744 = 1.0f.xx / float2(int2(spvTextureSize(_142, uint(0), _1741_dummy_parameter)));
                        float _1929;
                        _1929 = 0.0f;
                        float _1938;
                        for (int _1928 = -1; _1928 <= 1; _1929 = _1938, _1928++)
                        {
                            _1938 = _1929;
                            float _1969;
                            for (int _1930 = -1; _1930 <= 1; _1938 = _1969, _1930++)
                            {
                                float2 _1763 = _1693 + (float2(float(_1930), float(_1928)) * _1744);
                                bool _1766 = any(bool2(_1763.x < 0.0f.xx.x, _1763.y < 0.0f.xx.y));
                                bool _1773;
                                if (!_1766)
                                {
                                    _1773 = any(bool2(_1763.x > 1.0f.xx.x, _1763.y > 1.0f.xx.y));
                                }
                                else
                                {
                                    _1773 = _1766;
                                }
                                if (_1773)
                                {
                                    _1969 = _1938 + 1.0f;
                                }
                                else
                                {
                                    float _1935;
                                    do
                                    {
                                        if (_1649 == 0u)
                                        {
                                            _1935 = _142.SampleLevel(_146, _1763, 0.0f).x;
                                            break;
                                        }
                                        if (_1649 == 1u)
                                        {
                                            _1935 = _159.SampleLevel(_146, _1763, 0.0f).x;
                                            break;
                                        }
                                        if (_1649 == 2u)
                                        {
                                            _1935 = _171.SampleLevel(_146, _1763, 0.0f).x;
                                            break;
                                        }
                                        _1935 = _179.SampleLevel(_146, _1763, 0.0f).x;
                                        break;
                                    } while(false);
                                    _1969 = _1938 + float(_1736 <= _1935);
                                }
                            }
                        }
                        _1942 = _1929 * 0.111111111938953399658203125f;
                        break;
                    } while(false);
                    _1943 = _1942;
                }
                else
                {
                    _1943 = 1.0f;
                }
                _1955 = lerp(_1923, _1943, _1391);
            }
            else
            {
                _1955 = _1923;
            }
            _1956 = _1955;
            break;
        } while(false);
        _1957 = _1956;
    }
    else
    {
        _1957 = 1.0f;
    }
    float3 _985 = ((((((((1.0f.xxx - _841) * _847) * 0.3183098733425140380859375f.xxx) + ((_841 * (_772 / max((3.1415927410125732421875f * _780) * _780, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_752 * sqrt(_772 + ((_794 * _747) * _747))) + (_747 * sqrt(_772 + ((_794 * _752) * _752))), 9.9999999747524270787835121154785e-07f)))) * _66_m3.xyz) * _747) * _1957) + (((((_876.SampleLevel(_878, float2(_1901, acos(clamp(_1894.y, -1.0f, 1.0f)) * 0.3183098733425140380859375f), 0.0f).xyz * _847) * 0.3183098733425140380859375f.xxx) * lerp(1.0f, _583.x, clamp(_523_m2.z, 0.0f, 1.0f))) + (_898.SampleLevel(_878, float2(_1903, acos(clamp(_1246, -1.0f, 1.0f)) * 0.3183098733425140380859375f), _598 * max(_66_m4.z, 0.0f)).xyz * ((_827 * _933.x) + _933.y.xxx))) * max(_66_m4.x, 0.0f))) + (_523_m1.xyz * _865.SampleGrad(_538, _508, _510, _513).xyz);
    _987 = float4(clamp(_985, 0.0f.xxx, 65504.0f.xxx), (_523_m3.x == 2u) ? _549 : 1.0f);
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
    stage_output._987 = _987;
    return stage_output;
}
