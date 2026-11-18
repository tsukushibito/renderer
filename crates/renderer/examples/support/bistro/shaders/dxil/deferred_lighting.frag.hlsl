cbuffer _78_80 : register(b0, space0)
{
    row_major float4x4 _80_m0 : packoffset(c0);
    float4 _80_m1 : packoffset(c4);
    float4 _80_m2 : packoffset(c5);
    float4 _80_m3 : packoffset(c6);
    float4 _80_m4 : packoffset(c7);
    row_major float4x4 _80_m5[4] : packoffset(c8);
    float4 _80_m6 : packoffset(c24);
    float4 _80_m7 : packoffset(c25);
    float4 _80_m8 : packoffset(c26);
    float4 _80_m9 : packoffset(c27);
    float4 _80_m10 : packoffset(c28);
};

cbuffer _631_633 : register(b1, space0)
{
    row_major float4x4 _633_m0 : packoffset(c0);
};

Texture2D<float4> _156 : register(t0, space3);
SamplerState _160 : register(s4, space3);
Texture2D<float4> _173 : register(t1, space3);
Texture2D<float4> _185 : register(t2, space3);
Texture2D<float4> _193 : register(t3, space3);
Texture2D<float4> _596 : register(t3, space1);
SamplerState _598 : register(s4, space1);
Texture2D<float4> _681 : register(t0, space1);
Texture2D<float4> _688 : register(t1, space1);
Texture2D<float4> _695 : register(t2, space1);
Texture2D<float4> _871 : register(t0, space2);
SamplerState _873 : register(s3, space2);
Texture2D<float4> _893 : register(t1, space2);
Texture2D<float4> _904 : register(t2, space2);

static float4 gl_FragCoord;
static float4 _984;

struct SPIRV_Cross_Input
{
    float4 gl_FragCoord : SV_Position;
};

struct SPIRV_Cross_Output
{
    float4 _984 : SV_Target0;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

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
    uint _602_dummy_parameter;
    precise float2 _610 = gl_FragCoord.xy / float2(int2(spvTextureSize(_596, uint(0), _602_dummy_parameter)));
    float4 _616 = _596.SampleLevel(_598, _610, 0.0f);
    float _617 = _616.x;
    if (!(_617 < 1.0f))
    {
        discard;
    }
    precise float2 _627 = _610 * 2.0f;
    precise float2 _629 = _627 - 1.0f.xx;
    precise float4 _638 = _633_m0[0] * _629.x;
    precise float4 _644 = _633_m0[1] * _629.y;
    precise float4 _650 = _633_m0[2] * _617;
    precise float4 _654 = _638 + _644;
    precise float4 _658 = _654 + _650;
    precise float4 _664 = _658 + _633_m0[3];
    if (!(abs(_664.w) > 9.9999996826552253889678874634872e-21f))
    {
        discard;
    }
    float3 _679 = _664.xyz / _664.w.xxx;
    float4 _686 = _681.SampleLevel(_598, _610, 0.0f);
    float4 _693 = _688.SampleLevel(_598, _610, 0.0f);
    float4 _700 = _695.SampleLevel(_598, _610, 0.0f);
    float _704 = clamp(_686.w, 0.0f, 1.0f);
    float _708 = clamp(_693.z, 0.0f, 1.0f);
    float2 _719 = _693.xy;
    float _1020 = _693.x;
    float _1026 = (1.0f - abs(_1020)) - abs(_693.y);
    float3 _1029 = float3(_1020, _693.y, _1026);
    float3 _1945;
    if (_1026 < 0.0f)
    {
        bool2 _1047 = bool2(_719.x >= 0.0f.xx.x, _719.y >= 0.0f.xx.y);
        float2 _1055 = (1.0f.xx - abs(_693.yx)) * float2(_1047.x ? 1.0f.xx.x : (-1.0f).xx.x, _1047.y ? 1.0f.xx.y : (-1.0f).xx.y);
        float3 _1906 = _1029;
        _1906.x = _1055.x;
        _1906.y = _1055.y;
        _1945 = _1906;
    }
    else
    {
        _1945 = _1029;
    }
    float _1062 = dot(_1945, _1945);
    float3 _1946;
    if (_1062 > 9.9999996826552253889678874634872e-21f)
    {
        _1946 = _1945 * rsqrt(_1062);
    }
    else
    {
        _1946 = float3(0.0f, 0.0f, 1.0f);
    }
    uint _1081 = spvPackHalf2x16(float2(_693.w, 0.0f));
    float2 _1093 = (float2(float(_1081 & 127u), float(((_1081 - 1024u) & 16383u) >> 7u)) * 0.015748031437397003173828125f) - 1.0f.xx;
    float _1104 = _1093.x;
    float _1110 = (1.0f - abs(_1104)) - abs(_1093.y);
    float3 _1113 = float3(_1104, _1093.y, _1110);
    float3 _1947;
    if (_1110 < 0.0f)
    {
        bool2 _1131 = bool2(_1093.x >= 0.0f.xx.x, _1093.y >= 0.0f.xx.y);
        float2 _1139 = (1.0f.xx - abs(_1093.yx)) * float2(_1131.x ? 1.0f.xx.x : (-1.0f).xx.x, _1131.y ? 1.0f.xx.y : (-1.0f).xx.y);
        float3 _1914 = _1113;
        _1914.x = _1139.x;
        _1914.y = _1139.y;
        _1947 = _1914;
    }
    else
    {
        _1947 = _1113;
    }
    float _1146 = dot(_1947, _1947);
    float3 _1948;
    if (_1146 > 9.9999996826552253889678874634872e-21f)
    {
        _1948 = _1947 * rsqrt(_1146);
    }
    else
    {
        _1948 = float3(0.0f, 0.0f, 1.0f);
    }
    float3 _731 = _80_m1.xyz - _679;
    float _1164 = dot(_731, _731);
    float3 _1949;
    if (_1164 > 9.9999996826552253889678874634872e-21f)
    {
        _1949 = _731 * rsqrt(_1164);
    }
    else
    {
        _1949 = _1946;
    }
    float _1182 = dot(_80_m2.xyz, _80_m2.xyz);
    float3 _1950;
    if (_1182 > 9.9999996826552253889678874634872e-21f)
    {
        _1950 = _80_m2.xyz * rsqrt(_1182);
    }
    else
    {
        _1950 = _1946;
    }
    float3 _747 = _1949 + _1950;
    float _1200 = dot(_747, _747);
    float3 _1951;
    if (_1200 > 9.9999996826552253889678874634872e-21f)
    {
        _1951 = _747 * rsqrt(_1200);
    }
    else
    {
        _1951 = _1946;
    }
    float _756 = max(dot(_1946, _1950), 0.0f);
    float _761 = max(dot(_1946, _1949), 0.0f);
    float _766 = max(dot(_1946, _1951), 0.0f);
    float _777 = max(_708 * _708, 0.001000000047497451305389404296875f);
    float _781 = _777 * _777;
    float _789 = ((_766 * _766) * (_781 - 1.0f)) + 1.0f;
    float _803 = 1.0f - _781;
    float3 _833 = _686.xyz;
    float3 _836 = lerp(0.039999999105930328369140625f.xxx, _833, _704.xxx);
    float3 _850 = _836 + ((1.0f.xxx - _836) * pow(clamp(1.0f - max(dot(_1949, _1951), 0.0f), 0.0f, 1.0f), 5.0f));
    float3 _856 = _833 * (1.0f - _704);
    float _1221 = cos(_80_m4.y);
    float _1224 = sin(_80_m4.y);
    float _1233 = (_1221 * _1946.x) - (_1224 * _1946.z);
    float _1244 = (_1224 * _1946.x) + (_1221 * _1946.z);
    float2 _1247 = float3(_1233, _1946.y, _1244).xz;
    float _1952;
    if (dot(_1247, _1247) > 9.9999996826552253889678874634872e-21f)
    {
        _1952 = frac(0.5f + (atan2(_1244, _1233) * 0.15915493667125701904296875f));
    }
    else
    {
        _1952 = 0.5f;
    }
    float3 _885 = reflect(-_1949, _1946);
    float _1287 = _885.x;
    float _1291 = _885.z;
    float _1293 = (_1221 * _1287) - (_1224 * _1291);
    float _1295 = _885.y;
    float _1304 = (_1224 * _1287) + (_1221 * _1291);
    float2 _1307 = float3(_1293, _1295, _1304).xz;
    float _1953;
    if (dot(_1307, _1307) > 9.9999996826552253889678874634872e-21f)
    {
        _1953 = frac(0.5f + (atan2(_1304, _1293) * 0.15915493667125701904296875f));
    }
    else
    {
        _1953 = 0.5f;
    }
    uint _909_dummy_parameter;
    float2 _917 = 0.5f.xx / float2(int2(spvTextureSize(_904, uint(0), _909_dummy_parameter)));
    float4 _928 = _904.SampleLevel(_873, clamp(float2(_761, _708), _917, 1.0f.xx - _917), 0.0f);
    float _2007;
    if (_756 != 0.0f)
    {
        float _2006;
        do
        {
            if (!(_80_m4.w > 0.0f))
            {
                _2006 = 1.0f;
                break;
            }
            float _1371 = dot(_679 - _80_m1.xyz, _80_m9.xyz);
            bool _1375 = _1371 < _80_m1.w;
            bool _1383;
            if (!_1375)
            {
                _1383 = _1371 > _80_m6.w;
            }
            else
            {
                _1383 = _1375;
            }
            if (_1383)
            {
                _2006 = 1.0f;
                break;
            }
            uint _1955;
            if (_1371 <= _80_m6.x)
            {
                _1955 = 0u;
            }
            else
            {
                uint _1954;
                if (_1371 <= _80_m6.y)
                {
                    _1954 = 1u;
                }
                else
                {
                    _1954 = (_1371 <= _80_m6.z) ? 2u : 3u;
                }
                _1955 = _1954;
            }
            float _1493;
            float _1501;
            float _1973;
            do
            {
                uint _1488 = min(_1955, 3u);
                _1493 = 1.0f - max(dot(_1948, _1950), 0.0f);
                _1501 = _80_m10.z;
                float4 _1515 = mul(float4(_679 + (_1948 * ((_80_m7[_1488] * _1501) * _1493)), 1.0f), _80_m5[_1488]);
                float _1517 = _1515.w;
                if (!(_1517 > 9.9999996826552253889678874634872e-21f))
                {
                    _1973 = 1.0f;
                    break;
                }
                float3 _1527 = _1515.xyz / _1517.xxx;
                float2 _1532 = (_1527.xy * 0.5f) + 0.5f.xx;
                bool _1535 = any(bool2(_1532.x < 0.0f.xx.x, _1532.y < 0.0f.xx.y));
                bool _1542;
                if (!_1535)
                {
                    _1542 = any(bool2(_1532.x > 1.0f.xx.x, _1532.y > 1.0f.xx.y));
                }
                else
                {
                    _1542 = _1535;
                }
                bool _1549;
                if (!_1542)
                {
                    _1549 = _1527.z < 0.0f;
                }
                else
                {
                    _1549 = _1542;
                }
                bool _1556;
                if (!_1549)
                {
                    _1556 = _1527.z > 1.0f;
                }
                else
                {
                    _1556 = _1549;
                }
                if (_1556)
                {
                    _1973 = 1.0f;
                    break;
                }
                float _1575 = _1527.z - ((_80_m7[_1488] * _80_m8[_1488]) * (_80_m10.x + (_80_m10.y * _1493)));
                uint _1580_dummy_parameter;
                float2 _1583 = 1.0f.xx / float2(int2(spvTextureSize(_156, uint(0), _1580_dummy_parameter)));
                float _1960;
                _1960 = 0.0f;
                float _1969;
                for (int _1959 = -1; _1959 <= 1; _1960 = _1969, _1959++)
                {
                    _1969 = _1960;
                    float _2013;
                    for (int _1961 = -1; _1961 <= 1; _1969 = _2013, _1961++)
                    {
                        float2 _1602 = _1532 + (float2(float(_1961), float(_1959)) * _1583);
                        bool _1605 = any(bool2(_1602.x < 0.0f.xx.x, _1602.y < 0.0f.xx.y));
                        bool _1612;
                        if (!_1605)
                        {
                            _1612 = any(bool2(_1602.x > 1.0f.xx.x, _1602.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _1612 = _1605;
                        }
                        if (_1612)
                        {
                            _2013 = _1969 + 1.0f;
                        }
                        else
                        {
                            float _1966;
                            do
                            {
                                if (_1488 == 0u)
                                {
                                    _1966 = _156.SampleLevel(_160, _1602, 0.0f).x;
                                    break;
                                }
                                if (_1488 == 1u)
                                {
                                    _1966 = _173.SampleLevel(_160, _1602, 0.0f).x;
                                    break;
                                }
                                if (_1488 == 2u)
                                {
                                    _1966 = _185.SampleLevel(_160, _1602, 0.0f).x;
                                    break;
                                }
                                _1966 = _193.SampleLevel(_160, _1602, 0.0f).x;
                                break;
                            } while(false);
                            _2013 = _1969 + float(_1575 <= _1966);
                        }
                    }
                }
                _1973 = _1960 * 0.111111111938953399658203125f;
                break;
            } while(false);
            float _1974;
            if (_1955 == 0u)
            {
                _1974 = _80_m1.w;
            }
            else
            {
                _1974 = _80_m6[max(_1955, 1u) - 1u];
            }
            float _1435 = max((_80_m6[_1955] - _1974) * clamp(_80_m9.w, 0.0f, 1.0f), 9.9999999747524270787835121154785e-07f);
            float _1443 = clamp((_1371 - (_80_m6[_1955] - _1435)) / _1435, 0.0f, 1.0f);
            float _2005;
            if (_1443 > 0.0f)
            {
                float _1993;
                if (_1955 < 3u)
                {
                    float _1992;
                    do
                    {
                        uint _1702 = min((_1955 + 1u), 3u);
                        float4 _1729 = mul(float4(_679 + (_1948 * ((_80_m7[_1702] * _1501) * _1493)), 1.0f), _80_m5[_1702]);
                        float _1731 = _1729.w;
                        if (!(_1731 > 9.9999996826552253889678874634872e-21f))
                        {
                            _1992 = 1.0f;
                            break;
                        }
                        float3 _1741 = _1729.xyz / _1731.xxx;
                        float2 _1746 = (_1741.xy * 0.5f) + 0.5f.xx;
                        bool _1749 = any(bool2(_1746.x < 0.0f.xx.x, _1746.y < 0.0f.xx.y));
                        bool _1756;
                        if (!_1749)
                        {
                            _1756 = any(bool2(_1746.x > 1.0f.xx.x, _1746.y > 1.0f.xx.y));
                        }
                        else
                        {
                            _1756 = _1749;
                        }
                        bool _1763;
                        if (!_1756)
                        {
                            _1763 = _1741.z < 0.0f;
                        }
                        else
                        {
                            _1763 = _1756;
                        }
                        bool _1770;
                        if (!_1763)
                        {
                            _1770 = _1741.z > 1.0f;
                        }
                        else
                        {
                            _1770 = _1763;
                        }
                        if (_1770)
                        {
                            _1992 = 1.0f;
                            break;
                        }
                        float _1789 = _1741.z - ((_80_m7[_1702] * _80_m8[_1702]) * (_80_m10.x + (_80_m10.y * _1493)));
                        uint _1794_dummy_parameter;
                        float2 _1797 = 1.0f.xx / float2(int2(spvTextureSize(_156, uint(0), _1794_dummy_parameter)));
                        float _1979;
                        _1979 = 0.0f;
                        float _1988;
                        for (int _1978 = -1; _1978 <= 1; _1979 = _1988, _1978++)
                        {
                            _1988 = _1979;
                            float _2019;
                            for (int _1980 = -1; _1980 <= 1; _1988 = _2019, _1980++)
                            {
                                float2 _1816 = _1746 + (float2(float(_1980), float(_1978)) * _1797);
                                bool _1819 = any(bool2(_1816.x < 0.0f.xx.x, _1816.y < 0.0f.xx.y));
                                bool _1826;
                                if (!_1819)
                                {
                                    _1826 = any(bool2(_1816.x > 1.0f.xx.x, _1816.y > 1.0f.xx.y));
                                }
                                else
                                {
                                    _1826 = _1819;
                                }
                                if (_1826)
                                {
                                    _2019 = _1988 + 1.0f;
                                }
                                else
                                {
                                    float _1985;
                                    do
                                    {
                                        if (_1702 == 0u)
                                        {
                                            _1985 = _156.SampleLevel(_160, _1816, 0.0f).x;
                                            break;
                                        }
                                        if (_1702 == 1u)
                                        {
                                            _1985 = _173.SampleLevel(_160, _1816, 0.0f).x;
                                            break;
                                        }
                                        if (_1702 == 2u)
                                        {
                                            _1985 = _185.SampleLevel(_160, _1816, 0.0f).x;
                                            break;
                                        }
                                        _1985 = _193.SampleLevel(_160, _1816, 0.0f).x;
                                        break;
                                    } while(false);
                                    _2019 = _1988 + float(_1789 <= _1985);
                                }
                            }
                        }
                        _1992 = _1979 * 0.111111111938953399658203125f;
                        break;
                    } while(false);
                    _1993 = _1992;
                }
                else
                {
                    _1993 = 1.0f;
                }
                _2005 = lerp(_1973, _1993, _1443);
            }
            else
            {
                _2005 = _1973;
            }
            _2006 = _2005;
            break;
        } while(false);
        _2007 = _2006;
    }
    else
    {
        _2007 = 1.0f;
    }
    float3 _982 = ((((((((1.0f.xxx - _850) * _856) * 0.3183098733425140380859375f.xxx) + ((_850 * (_781 / max((3.1415927410125732421875f * _789) * _789, 9.9999999600419720025001879548654e-13f))) * (0.5f / max((_761 * sqrt(_781 + ((_803 * _756) * _756))) + (_756 * sqrt(_781 + ((_803 * _761) * _761))), 9.9999999747524270787835121154785e-07f)))) * _80_m3.xyz) * _756) * _2007) + (((((_871.SampleLevel(_873, float2(_1952, acos(clamp(_1946.y, -1.0f, 1.0f)) * 0.3183098733425140380859375f), 0.0f).xyz * _856) * 0.3183098733425140380859375f.xxx) * clamp(_700.w, 0.0f, 1.0f)) + (_893.SampleLevel(_873, float2(_1953, acos(clamp(_1295, -1.0f, 1.0f)) * 0.3183098733425140380859375f), _708 * max(_80_m4.z, 0.0f)).xyz * ((_836 * _928.x) + _928.y.xxx))) * max(_80_m4.x, 0.0f))) + _700.xyz;
    _984 = float4(clamp(_982, 0.0f.xxx, 65504.0f.xxx), 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._984 = _984;
    return stage_output;
}
