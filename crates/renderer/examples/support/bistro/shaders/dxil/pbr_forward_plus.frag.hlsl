struct _92
{
    float4 _m0;
    float4 _m1;
};

ByteAddressBuffer _97 : register(t2, space0);
ByteAddressBuffer _380 : register(t3, space0);
cbuffer _432_434 : register(b0, space0)
{
    row_major float4x4 _434_m0 : packoffset(c0);
    float4 _434_m1 : packoffset(c4);
    float4 _434_m2 : packoffset(c5);
    float4 _434_m3 : packoffset(c6);
    float4 _434_m4 : packoffset(c7);
    row_major float4x4 _434_m5[4] : packoffset(c8);
    float4 _434_m6 : packoffset(c24);
    float4 _434_m7 : packoffset(c25);
    float4 _434_m8 : packoffset(c26);
    float4 _434_m9 : packoffset(c27);
    float4 _434_m10 : packoffset(c28);
};

cbuffer _874_876 : register(b1, space0)
{
    float4 _876_m0 : packoffset(c0);
    float4 _876_m1 : packoffset(c1);
    float4 _876_m2 : packoffset(c2);
    uint4 _876_m3 : packoffset(c3);
};

Texture2D<float4> _503 : register(t0, space3);
SamplerState _507 : register(s4, space3);
Texture2D<float4> _519 : register(t1, space3);
Texture2D<float4> _531 : register(t2, space3);
Texture2D<float4> _539 : register(t3, space3);
Texture2D<float4> _889 : register(t0, space1);
SamplerState _891 : register(s4, space1);
Texture2D<float4> _929 : register(t2, space1);
Texture2D<float4> _1023 : register(t1, space1);
Texture2D<float4> _1213 : register(t3, space1);
Texture2D<float4> _1224 : register(t0, space2);
SamplerState _1226 : register(s3, space2);
Texture2D<float4> _1246 : register(t1, space2);
Texture2D<float4> _1257 : register(t2, space2);

static float4 gl_FragCoord;
static bool gl_FrontFacing;
static float3 _563;
static float2 _862;
static float3 _960;
static float4 _977;
static float4 _1350;

struct SPIRV_Cross_Input
{
    float3 _563 : TEXCOORD0;
    float3 _960 : TEXCOORD1;
    float4 _977 : TEXCOORD2;
    float2 _862 : TEXCOORD3;
    float4 gl_FragCoord : SV_Position;
    bool gl_FrontFacing : SV_IsFrontFace;
};

struct SPIRV_Cross_Output
{
    float4 _1350 : SV_Target0;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

void frag_main()
{
    float2 _864 = ddx(_862);
    float2 _867 = ddy(_862);
    bool _871 = !gl_FrontFacing;
    bool _882;
    if (_871)
    {
        _882 = _876_m3.y == 0u;
    }
    else
    {
        _882 = _871;
    }
    if (_882)
    {
        discard;
    }
    float4 _898 = _876_m0 * _889.SampleGrad(_891, _862, _864, _867);
    float _902 = clamp(_898.w, 0.0f, 1.0f);
    bool _907 = _876_m3.x == 1u;
    bool _915;
    if (_907)
    {
        _915 = _902 < _876_m2.w;
    }
    else
    {
        _915 = _907;
    }
    bool _924;
    if (!_915)
    {
        _924 = (_876_m3.x == 2u) && (_902 <= 0.0f);
    }
    else
    {
        _924 = _915;
    }
    if (_924)
    {
        discard;
    }
    float4 _936 = _929.SampleGrad(_891, _862, _864, _867);
    float _944 = clamp(_876_m2.x * _936.z, 0.0f, 1.0f);
    float _951 = clamp(_876_m2.y * _936.y, 0.0f, 1.0f);
    float _1393 = dot(_960, _960);
    float3 _2592;
    if (_1393 > 9.9999996826552253889678874634872e-21f)
    {
        _2592 = _960 * rsqrt(_1393);
    }
    else
    {
        _2592 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _2593;
    if (gl_FrontFacing)
    {
        _2593 = _2592;
    }
    else
    {
        _2593 = -_2592;
    }
    float3 _986 = _977.xyz - (_2592 * dot(_2592, _977.xyz));
    float3 _2597;
    if (abs(_2592.z) < 0.89999997615814208984375f)
    {
        float3 _996 = cross(float3(0.0f, 0.0f, 1.0f), _2592);
        float _1411 = dot(_996, _996);
        float3 _2596;
        if (_1411 > 9.9999996826552253889678874634872e-21f)
        {
            _2596 = _996 * rsqrt(_1411);
        }
        else
        {
            _2596 = float3(1.0f, 0.0f, 0.0f);
        }
        _2597 = _2596;
    }
    else
    {
        float3 _1004 = cross(float3(0.0f, 1.0f, 0.0f), _2592);
        float _1429 = dot(_1004, _1004);
        float3 _2595;
        if (_1429 > 9.9999996826552253889678874634872e-21f)
        {
            _2595 = _1004 * rsqrt(_1429);
        }
        else
        {
            _2595 = float3(1.0f, 0.0f, 0.0f);
        }
        _2597 = _2595;
    }
    float _1447 = dot(_986, _986);
    float3 _2598;
    if (_1447 > 9.9999996826552253889678874634872e-21f)
    {
        _2598 = _986 * rsqrt(_1447);
    }
    else
    {
        _2598 = _2597;
    }
    float3 _1035 = (_1023.SampleGrad(_891, _862, _864, _867).xyz * 2.0f) - 1.0f.xxx;
    float2 _1040 = _1035.xy * _876_m1.w;
    float3 _1058 = ((_2598 * _1040.x) + ((cross(_2592, _2598) * ((_977.w < 0.0f) ? (-1.0f) : 1.0f)) * _1040.y)) + (_2592 * _1035.z);
    float _1465 = dot(_1058, _1058);
    float3 _2603;
    if (_1465 > 9.9999996826552253889678874634872e-21f)
    {
        _2603 = _1058 * rsqrt(_1465);
    }
    else
    {
        _2603 = _2592;
    }
    float3 _2604;
    if (_871)
    {
        _2604 = -_2603;
    }
    else
    {
        _2604 = _2603;
    }
    float3 _1074 = _434_m1.xyz - _563;
    float _1483 = dot(_1074, _1074);
    float3 _2605;
    if (_1483 > 9.9999996826552253889678874634872e-21f)
    {
        _2605 = _1074 * rsqrt(_1483);
    }
    else
    {
        _2605 = _2604;
    }
    float _1501 = dot(_434_m2.xyz, _434_m2.xyz);
    float3 _2607;
    if (_1501 > 9.9999996826552253889678874634872e-21f)
    {
        _2607 = _434_m2.xyz * rsqrt(_1501);
    }
    else
    {
        _2607 = _2604;
    }
    float3 _1090 = _2605 + _2607;
    float _1519 = dot(_1090, _1090);
    float3 _2609;
    if (_1519 > 9.9999996826552253889678874634872e-21f)
    {
        _2609 = _1090 * rsqrt(_1519);
    }
    else
    {
        _2609 = _2604;
    }
    float _1099 = max(dot(_2604, _2607), 0.0f);
    float _1104 = max(dot(_2604, _2605), 0.0f);
    float _1109 = max(dot(_2604, _2609), 0.0f);
    float _1119 = max(_951 * _951, 0.001000000047497451305389404296875f);
    float _1123 = _1119 * _1119;
    float _1129 = _1123 - 1.0f;
    float _1131 = ((_1109 * _1109) * _1129) + 1.0f;
    float _1144 = 1.0f - _1123;
    float _1162 = sqrt(_1123 + ((_1144 * _1104) * _1104));
    float3 _1174 = _898.xyz;
    float3 _1177 = lerp(0.039999999105930328369140625f.xxx, _1174, _944.xxx);
    float3 _1186 = 1.0f.xxx - _1177;
    float3 _1189 = _1177 + (_1186 * pow(clamp(1.0f - max(dot(_2605, _2609), 0.0f), 0.0f, 1.0f), 5.0f));
    float3 _1195 = _1174 * (1.0f - _944);
    float _1540 = cos(_434_m4.y);
    float _1543 = sin(_434_m4.y);
    float _1552 = (_1540 * _2604.x) - (_1543 * _2604.z);
    float _1563 = (_1543 * _2604.x) + (_1540 * _2604.z);
    float2 _1566 = float3(_1552, _2604.y, _1563).xz;
    float _2611;
    if (dot(_1566, _1566) > 9.9999996826552253889678874634872e-21f)
    {
        _2611 = frac(0.5f + (atan2(_1563, _1552) * 0.15915493667125701904296875f));
    }
    else
    {
        _2611 = 0.5f;
    }
    float3 _1238 = reflect(-_2605, _2604);
    float _1606 = _1238.x;
    float _1610 = _1238.z;
    float _1612 = (_1540 * _1606) - (_1543 * _1610);
    float _1614 = _1238.y;
    float _1623 = (_1543 * _1606) + (_1540 * _1610);
    float2 _1626 = float3(_1612, _1614, _1623).xz;
    float _2613;
    if (dot(_1626, _1626) > 9.9999996826552253889678874634872e-21f)
    {
        _2613 = frac(0.5f + (atan2(_1623, _1612) * 0.15915493667125701904296875f));
    }
    else
    {
        _2613 = 0.5f;
    }
    uint _1262_dummy_parameter;
    float2 _1270 = 0.5f.xx / float2(int2(spvTextureSize(_1257, uint(0), _1262_dummy_parameter)));
    float4 _1281 = _1257.SampleLevel(_1226, clamp(float2(_1104, _951), _1270, 1.0f.xx - _1270), 0.0f);
    float _2667;
    if (_1099 != 0.0f)
    {
        float _2666;
        do
        {
            if (!(_434_m4.w > 0.0f))
            {
                _2666 = 1.0f;
                break;
            }
            float _1688 = dot(_563 - _434_m1.xyz, _434_m9.xyz);
            bool _1692 = _1688 < _434_m1.w;
            bool _1700;
            if (!_1692)
            {
                _1700 = _1688 > _434_m6.w;
            }
            else
            {
                _1700 = _1692;
            }
            if (_1700)
            {
                _2666 = 1.0f;
                break;
            }
            uint _2615;
            if (_1688 <= _434_m6.x)
            {
                _2615 = 0u;
            }
            else
            {
                uint _2614;
                if (_1688 <= _434_m6.y)
                {
                    _2614 = 1u;
                }
                else
                {
                    _2614 = (_1688 <= _434_m6.z) ? 2u : 3u;
                }
                _2615 = _2614;
            }
            float _1808;
            float _1816;
            float _2633;
            do
            {
                uint _1803 = min(_2615, 3u);
                _1808 = 1.0f - max(dot(_2593, _2607), 0.0f);
                _1816 = _434_m10.z;
                float4 _1830 = mul(float4(_563 + (_2593 * ((_434_m7[_1803] * _1816) * _1808)), 1.0f), _434_m5[_1803]);
                float _1832 = _1830.w;
                if (!(_1832 > 9.9999996826552253889678874634872e-21f))
                {
                    _2633 = 1.0f;
                    break;
                }
                float3 _1842 = _1830.xyz / _1832.xxx;
                float2 _1847 = (_1842.xy * 0.5f) + 0.5f.xx;
                bool _1850 = any(bool2(_1847.x < 0.0f.xx.x, _1847.y < 0.0f.xx.y));
                bool _1857;
                if (!_1850)
                {
                    _1857 = any(bool2(_1847.x > 1.0f.xx.x, _1847.y > 1.0f.xx.y));
                }
                else
                {
                    _1857 = _1850;
                }
                bool _1864;
                if (!_1857)
                {
                    _1864 = _1842.z < 0.0f;
                }
                else
                {
                    _1864 = _1857;
                }
                bool _1871;
                if (!_1864)
                {
                    _1871 = _1842.z > 1.0f;
                }
                else
                {
                    _1871 = _1864;
                }
                if (_1871)
                {
                    _2633 = 1.0f;
                    break;
                }
                float _1890 = _1842.z - ((_434_m7[_1803] * _434_m8[_1803]) * (_434_m10.x + (_434_m10.y * _1808)));
                uint _1895_dummy_parameter;
                float2 _1898 = 1.0f.xx / float2(int2(spvTextureSize(_503, uint(0), _1895_dummy_parameter)));
                float _2620;
                _2620 = 0.0f;
                float _2629;
                for (int _2619 = -1; _2619 <= 1; _2620 = _2629, _2619++)
                {
                    _2629 = _2620;
                    float _2715;
                    for (int _2621 = -1; _2621 <= 1; _2629 = _2715, _2621++)
                    {
                        float2 _1917 = _1847 + (float2(float(_2621), float(_2619)) * _1898);
                        bool _1920 = any(bool2(_1917.x < 0.0f.xx.x, _1917.y < 0.0f.xx.y));
                        bool _1927;
                        if (!_1920)
                        {
                            _1927 = any(bool2(_1917.x > 1.0f.xx.x, _1917.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _1927 = _1920;
                        }
                        if (_1927)
                        {
                            _2715 = _2629 + 1.0f;
                        }
                        else
                        {
                            float _2626;
                            do
                            {
                                if (_1803 == 0u)
                                {
                                    _2626 = _503.SampleLevel(_507, _1917, 0.0f).x;
                                    break;
                                }
                                if (_1803 == 1u)
                                {
                                    _2626 = _519.SampleLevel(_507, _1917, 0.0f).x;
                                    break;
                                }
                                if (_1803 == 2u)
                                {
                                    _2626 = _531.SampleLevel(_507, _1917, 0.0f).x;
                                    break;
                                }
                                _2626 = _539.SampleLevel(_507, _1917, 0.0f).x;
                                break;
                            } while(false);
                            _2715 = _2629 + float(_1890 <= _2626);
                        }
                    }
                }
                _2633 = _2620 * 0.111111111938953399658203125f;
                break;
            } while(false);
            float _2634;
            if (_2615 == 0u)
            {
                _2634 = _434_m1.w;
            }
            else
            {
                _2634 = _434_m6[max(_2615, 1u) - 1u];
            }
            float _1751 = max((_434_m6[_2615] - _2634) * clamp(_434_m9.w, 0.0f, 1.0f), 9.9999999747524270787835121154785e-07f);
            float _1759 = clamp((_1688 - (_434_m6[_2615] - _1751)) / _1751, 0.0f, 1.0f);
            float _2665;
            if (_1759 > 0.0f)
            {
                float _2653;
                if (_2615 < 3u)
                {
                    float _2652;
                    do
                    {
                        uint _2017 = min((_2615 + 1u), 3u);
                        float4 _2044 = mul(float4(_563 + (_2593 * ((_434_m7[_2017] * _1816) * _1808)), 1.0f), _434_m5[_2017]);
                        float _2046 = _2044.w;
                        if (!(_2046 > 9.9999996826552253889678874634872e-21f))
                        {
                            _2652 = 1.0f;
                            break;
                        }
                        float3 _2056 = _2044.xyz / _2046.xxx;
                        float2 _2061 = (_2056.xy * 0.5f) + 0.5f.xx;
                        bool _2064 = any(bool2(_2061.x < 0.0f.xx.x, _2061.y < 0.0f.xx.y));
                        bool _2071;
                        if (!_2064)
                        {
                            _2071 = any(bool2(_2061.x > 1.0f.xx.x, _2061.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _2071 = _2064;
                        }
                        bool _2078;
                        if (!_2071)
                        {
                            _2078 = _2056.z < 0.0f;
                        }
                        else
                        {
                            _2078 = _2071;
                        }
                        bool _2085;
                        if (!_2078)
                        {
                            _2085 = _2056.z > 1.0f;
                        }
                        else
                        {
                            _2085 = _2078;
                        }
                        if (_2085)
                        {
                            _2652 = 1.0f;
                            break;
                        }
                        float _2104 = _2056.z - ((_434_m7[_2017] * _434_m8[_2017]) * (_434_m10.x + (_434_m10.y * _1808)));
                        uint _2109_dummy_parameter;
                        float2 _2112 = 1.0f.xx / float2(int2(spvTextureSize(_503, uint(0), _2109_dummy_parameter)));
                        float _2639;
                        _2639 = 0.0f;
                        float _2648;
                        for (int _2638 = -1; _2638 <= 1; _2639 = _2648, _2638++)
                        {
                            _2648 = _2639;
                            float _2721;
                            for (int _2640 = -1; _2640 <= 1; _2648 = _2721, _2640++)
                            {
                                float2 _2131 = _2061 + (float2(float(_2640), float(_2638)) * _2112);
                                bool _2134 = any(bool2(_2131.x < 0.0f.xx.x, _2131.y < 0.0f.xx.y));
                                bool _2141;
                                if (!_2134)
                                {
                                    _2141 = any(bool2(_2131.x > 1.0f.xx.x, _2131.y > 1.0f.xx.y));
                                }
                                else
                                {
                                    _2141 = _2134;
                                }
                                if (_2141)
                                {
                                    _2721 = _2648 + 1.0f;
                                }
                                else
                                {
                                    float _2645;
                                    do
                                    {
                                        if (_2017 == 0u)
                                        {
                                            _2645 = _503.SampleLevel(_507, _2131, 0.0f).x;
                                            break;
                                        }
                                        if (_2017 == 1u)
                                        {
                                            _2645 = _519.SampleLevel(_507, _2131, 0.0f).x;
                                            break;
                                        }
                                        if (_2017 == 2u)
                                        {
                                            _2645 = _531.SampleLevel(_507, _2131, 0.0f).x;
                                            break;
                                        }
                                        _2645 = _539.SampleLevel(_507, _2131, 0.0f).x;
                                        break;
                                    } while(false);
                                    _2721 = _2648 + float(_2104 <= _2645);
                                }
                            }
                        }
                        _2652 = _2639 * 0.111111111938953399658203125f;
                        break;
                    } while(false);
                    _2653 = _2652;
                }
                else
                {
                    _2653 = 1.0f;
                }
                _2665 = lerp(_2633, _2653, _1759);
            }
            else
            {
                _2665 = _2633;
            }
            _2666 = _2665;
            break;
        } while(false);
        _2667 = _2666;
    }
    else
    {
        _2667 = 1.0f;
    }
    float3 _1333 = ((((((((1.0f.xxx - _1189) * _1195) * 0.3183098733425140380859375f.xxx) + ((_1189 * (_1123 / max((3.1415927410125732421875f * _1131) * _1131, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_1104 * sqrt(_1123 + ((_1144 * _1099) * _1099))) + (_1099 * _1162), 9.9999999747524270787835121154785e-07f)))) * _434_m3.xyz) * _1099) * _2667) + (((((_1224.SampleLevel(_1226, float2(_2611, acos(clamp(_2604.y, -1.0f, 1.0f)) * 0.3183098733425140380859375f), 0.0f).xyz * _1195) * 0.3183098733425140380859375f.xxx) * lerp(1.0f, _936.x, clamp(_876_m2.z, 0.0f, 1.0f))) + (_1246.SampleLevel(_1226, float2(_2613, acos(clamp(_1614, -1.0f, 1.0f)) * 0.3183098733425140380859375f), _951 * max(_434_m4.z, 0.0f)).xyz * ((_1177 * _1281.x) + _1281.y.xxx))) * max(_434_m4.x, 0.0f))) + (_876_m1.xyz * _1213.SampleGrad(_891, _862, _864, _867).xyz);
    uint _2294 = min(_97.Load(8), 64u);
    uint2 _2300 = clamp(_97.Load4(0).xy, uint2(1u, 1u), uint2(65535u, 65535u));
    uint2 _2242 = uint2(clamp(gl_FragCoord.xy, 0.0f.xx, float2(_2300 - uint2(1u, 1u)))) / uint2(16u, 16u);
    uint _2253 = min(((_2242.y * ((_2300.x + 15u) / 16u)) + _2242.x), 65535u) * 65u;
    uint _2257 = min(_380.Load(_2253 * 4 + 0), 64u);
    float3 _2696;
    _2696 = 0.0f.xxx;
    float3 _2722;
    for (uint _2695 = 0u; _2695 < _2257; _2696 = _2722, _2695++)
    {
        uint _2267 = (_2253 + 1u) + _2695;
        if (_380.Load(_2267 * 4 + 0) < _2294)
        {
            float3 _2700;
            do
            {
                _92 _2344;
                _2344._m0 = asfloat(_97.Load4(_380.Load(_2267 * 4 + 0) * 32 + 16));
                _2344._m1 = asfloat(_97.Load4(_380.Load(_2267 * 4 + 0) * 32 + 32));
                float3 _2353 = _2344._m0.xyz - _563;
                float _2356 = dot(_2353, _2353);
                bool _2361 = !(_2344._m0.w > 0.0f);
                bool _2371;
                if (!_2361)
                {
                    _2371 = !(_2356 < (_2344._m0.w * _2344._m0.w));
                }
                else
                {
                    _2371 = _2361;
                }
                if (_2371)
                {
                    _2700 = 0.0f.xxx;
                    break;
                }
                float3 _2698;
                if (_2356 > 9.9999996826552253889678874634872e-21f)
                {
                    _2698 = _2353 * rsqrt(_2356);
                }
                else
                {
                    _2698 = _2604;
                }
                float _2380 = max(dot(_2604, _2698), 0.0f);
                if (!(_2380 > 0.0f))
                {
                    _2700 = 0.0f.xxx;
                    break;
                }
                float3 _2388 = _2605 + _2698;
                float _2531 = dot(_2388, _2388);
                float3 _2699;
                if (_2531 > 9.9999996826552253889678874634872e-21f)
                {
                    _2699 = _2388 * rsqrt(_2531);
                }
                else
                {
                    _2699 = _2604;
                }
                float _2398 = max(dot(_2604, _2699), 0.0f);
                float _2416 = ((_2398 * _2398) * _1129) + 1.0f;
                float3 _2460 = _1177 + (_1186 * pow(clamp(1.0f - max(dot(_2605, _2699), 0.0f), 0.0f, 1.0f), 5.0f));
                float _2476 = _2356 / (_2344._m0.w * _2344._m0.w);
                float _2481 = max(1.0f - (_2476 * _2476), 0.0f);
                _2700 = ((((((1.0f.xxx - _2460) * _1195) * 0.3183098733425140380859375f.xxx) + ((_2460 * (_1123 / max((3.1415927410125732421875f * _2416) * _2416, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_1104 * sqrt(_1123 + ((_1144 * _2380) * _2380))) + (_2380 * _1162), 9.9999999747524270787835121154785e-07f)))) * (max(_2344._m1.xyz, 0.0f.xxx) * max(_2344._m1.w, 0.0f))) * _2380) * ((_2481 * _2481) / max(_2356, 0.00999999977648258209228515625f));
                break;
            } while(false);
            _2722 = _2696 + _2700;
        }
        else
        {
            _2722 = _2696;
        }
    }
    _1350 = float4(clamp(_1333 + _2696, 0.0f.xxx, 65504.0f.xxx), (_876_m3.x == 2u) ? _902 : 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    gl_FrontFacing = stage_input.gl_FrontFacing;
    _563 = stage_input._563;
    _862 = stage_input._862;
    _960 = stage_input._960;
    _977 = stage_input._977;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._1350 = _1350;
    return stage_output;
}
