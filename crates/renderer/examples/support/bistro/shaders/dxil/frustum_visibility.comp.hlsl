struct _429
{
    uint4 _m0;
    uint4 _m1;
};

static const uint3 gl_WorkGroupSize = uint3(64u, 1u, 1u);

cbuffer _107_109 : register(b0, space0)
{
    row_major float4x4 _109_m0 : packoffset(c0);
};

ByteAddressBuffer _433 : register(t1, space0);
RWByteAddressBuffer _462 : register(u2, space0);

static uint3 gl_GlobalInvocationID;
struct SPIRV_Cross_Input
{
    uint3 gl_GlobalInvocationID : SV_DispatchThreadID;
};

void comp_main()
{
    uint _1139;
    if (_433.Load4(gl_GlobalInvocationID.x * 32 + 0).w != 0u)
    {
        float3 _452 = asfloat(_433.Load4(gl_GlobalInvocationID.x * 32 + 0).xyz);
        float3 _454 = asfloat(_433.Load4(gl_GlobalInvocationID.x * 32 + 16).xyz);
        bool _1138;
        do
        {
            uint3 _784 = _433.Load4(gl_GlobalInvocationID.x * 32 + 0).xyz & uint3(2139095040u, 2139095040u, 2139095040u);
            bool _786 = all(bool3(_784.x < uint3(2139095040u, 2139095040u, 2139095040u).x, _784.y < uint3(2139095040u, 2139095040u, 2139095040u).y, _784.z < uint3(2139095040u, 2139095040u, 2139095040u).z));
            bool _550;
            if (_786)
            {
                uint3 _791 = _433.Load4(gl_GlobalInvocationID.x * 32 + 16).xyz & uint3(2139095040u, 2139095040u, 2139095040u);
                _550 = !all(bool3(_791.x < uint3(2139095040u, 2139095040u, 2139095040u).x, _791.y < uint3(2139095040u, 2139095040u, 2139095040u).y, _791.z < uint3(2139095040u, 2139095040u, 2139095040u).z));
            }
            else
            {
                _550 = !_786;
            }
            bool _558;
            if (!_550)
            {
                _558 = any(bool3(_452.x > _454.x, _452.y > _454.y, _452.z > _454.z));
            }
            else
            {
                _558 = _550;
            }
            bool _565;
            if (!_558)
            {
                uint4 _798 = asuint(_109_m0[0]) & uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u);
                _565 = !all(bool4(_798.x < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).x, _798.y < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).y, _798.z < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).z, _798.w < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).w));
            }
            else
            {
                _565 = _558;
            }
            bool _572;
            if (!_565)
            {
                uint4 _805 = asuint(_109_m0[1]) & uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u);
                _572 = !all(bool4(_805.x < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).x, _805.y < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).y, _805.z < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).z, _805.w < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).w));
            }
            else
            {
                _572 = _565;
            }
            bool _579;
            if (!_572)
            {
                uint4 _812 = asuint(_109_m0[2]) & uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u);
                _579 = !all(bool4(_812.x < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).x, _812.y < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).y, _812.z < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).z, _812.w < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).w));
            }
            else
            {
                _579 = _572;
            }
            bool _586;
            if (!_579)
            {
                uint4 _819 = asuint(_109_m0[3]) & uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u);
                _586 = !all(bool4(_819.x < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).x, _819.y < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).y, _819.z < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).z, _819.w < uint4(2139095040u, 2139095040u, 2139095040u, 2139095040u).w));
            }
            else
            {
                _586 = _579;
            }
            if (_586)
            {
                _1138 = true;
                break;
            }
            float3 _593 = max(abs(_452), abs(_454));
            float4 _595 = abs(_109_m0[0]);
            float4 _597 = abs(_109_m0[1]);
            float4 _599 = abs(_109_m0[2]);
            float4 _601 = abs(_109_m0[3]);
            precise float4 _604 = _595 + _597;
            precise float4 _606 = _604 + _599;
            precise float4 _608 = _606 + _601;
            precise float4 _612 = _595 * _593.x;
            precise float4 _616 = _597 * _593.y;
            precise float4 _617 = _612 + _616;
            precise float4 _621 = _599 * _593.z;
            precise float4 _622 = _617 + _621;
            precise float4 _624 = _622 + _601;
            bool _627 = all(bool4(_608.x < 4.2535293329816107476463022935565e+37f.xxxx.x, _608.y < 4.2535293329816107476463022935565e+37f.xxxx.y, _608.z < 4.2535293329816107476463022935565e+37f.xxxx.z, _608.w < 4.2535293329816107476463022935565e+37f.xxxx.w));
            bool _636;
            if (_627)
            {
                _636 = !all(bool4(_624.x < 4.2535293329816107476463022935565e+37f.xxxx.x, _624.y < 4.2535293329816107476463022935565e+37f.xxxx.y, _624.z < 4.2535293329816107476463022935565e+37f.xxxx.z, _624.w < 4.2535293329816107476463022935565e+37f.xxxx.w));
            }
            else
            {
                _636 = !_627;
            }
            if (_636)
            {
                _1138 = true;
                break;
            }
            precise float3 _640 = _593 * 1.8807909613156600127499784595556e-37f;
            precise float _645 = _640.x + _640.y;
            precise float _648 = _645 + _640.z;
            precise float4 _651 = max(_624, 1.0f.xxxx) * 1.52587890625e-05f;
            precise float4 _653 = _608 * 1.8807909613156600127499784595556e-37f;
            precise float4 _656 = _653 + _648.xxxx;
            precise float4 _659 = _651 + _656;
            float4 _668 = float4(_109_m0[0].x, _109_m0[1].x, _109_m0[2].x, _109_m0[3].x);
            float4 _677 = float4(_109_m0[0].y, _109_m0[1].y, _109_m0[2].y, _109_m0[3].y);
            float4 _686 = float4(_109_m0[0].z, _109_m0[1].z, _109_m0[2].z, _109_m0[3].z);
            float4 _695 = float4(_109_m0[0].w, _109_m0[1].w, _109_m0[2].w, _109_m0[3].w);
            float4 _698 = _695 + _668;
            float _703 = _659.w + _659.x;
            float3 _832 = _698.xyz;
            bool3 _833 = bool3(_832.x >= 0.0f.xxx.x, _832.y >= 0.0f.xxx.y, _832.z >= 0.0f.xxx.z);
            precise float4 _841 = _698 * float4(float3(_833.x ? _454.x : _452.x, _833.y ? _454.y : _452.y, _833.z ? _454.z : _452.z), 1.0f);
            precise float _846 = _841.x + _841.y;
            precise float _850 = _846 + _841.z;
            precise float _854 = _850 + _841.w;
            bool _857 = !isinf(_854);
            bool _864;
            if (_857)
            {
                _864 = _854 < (-_703);
            }
            else
            {
                _864 = _857;
            }
            bool _721;
            if (!_864)
            {
                float4 _711 = _695 - _668;
                float3 _875 = _711.xyz;
                bool3 _876 = bool3(_875.x >= 0.0f.xxx.x, _875.y >= 0.0f.xxx.y, _875.z >= 0.0f.xxx.z);
                precise float4 _884 = _711 * float4(float3(_876.x ? _454.x : _452.x, _876.y ? _454.y : _452.y, _876.z ? _454.z : _452.z), 1.0f);
                precise float _889 = _884.x + _884.y;
                precise float _893 = _889 + _884.z;
                precise float _897 = _893 + _884.w;
                bool _900 = !isinf(_897);
                bool _907;
                if (_900)
                {
                    _907 = _897 < (-_703);
                }
                else
                {
                    _907 = _900;
                }
                _721 = _907;
            }
            else
            {
                _721 = _864;
            }
            bool _736;
            if (!_721)
            {
                float4 _726 = _695 + _677;
                float3 _918 = _726.xyz;
                bool3 _919 = bool3(_918.x >= 0.0f.xxx.x, _918.y >= 0.0f.xxx.y, _918.z >= 0.0f.xxx.z);
                precise float4 _927 = _726 * float4(float3(_919.x ? _454.x : _452.x, _919.y ? _454.y : _452.y, _919.z ? _454.z : _452.z), 1.0f);
                precise float _932 = _927.x + _927.y;
                precise float _936 = _932 + _927.z;
                precise float _940 = _936 + _927.w;
                bool _943 = !isinf(_940);
                bool _950;
                if (_943)
                {
                    _950 = _940 < (-(_659.w + _659.y));
                }
                else
                {
                    _950 = _943;
                }
                _736 = _950;
            }
            else
            {
                _736 = _721;
            }
            bool _751;
            if (!_736)
            {
                float4 _741 = _695 - _677;
                float3 _961 = _741.xyz;
                bool3 _962 = bool3(_961.x >= 0.0f.xxx.x, _961.y >= 0.0f.xxx.y, _961.z >= 0.0f.xxx.z);
                precise float4 _970 = _741 * float4(float3(_962.x ? _454.x : _452.x, _962.y ? _454.y : _452.y, _962.z ? _454.z : _452.z), 1.0f);
                precise float _975 = _970.x + _970.y;
                precise float _979 = _975 + _970.z;
                precise float _983 = _979 + _970.w;
                bool _986 = !isinf(_983);
                bool _993;
                if (_986)
                {
                    _993 = _983 < (-(_659.w + _659.y));
                }
                else
                {
                    _993 = _986;
                }
                _751 = _993;
            }
            else
            {
                _751 = _736;
            }
            bool _761;
            if (!_751)
            {
                float3 _1004 = _686.xyz;
                bool3 _1005 = bool3(_1004.x >= 0.0f.xxx.x, _1004.y >= 0.0f.xxx.y, _1004.z >= 0.0f.xxx.z);
                precise float4 _1013 = _686 * float4(float3(_1005.x ? _454.x : _452.x, _1005.y ? _454.y : _452.y, _1005.z ? _454.z : _452.z), 1.0f);
                precise float _1018 = _1013.x + _1013.y;
                precise float _1022 = _1018 + _1013.z;
                precise float _1026 = _1022 + _1013.w;
                bool _1029 = !isinf(_1026);
                bool _1036;
                if (_1029)
                {
                    _1036 = _1026 < (-_659.z);
                }
                else
                {
                    _1036 = _1029;
                }
                _761 = _1036;
            }
            else
            {
                _761 = _751;
            }
            bool _776;
            if (!_761)
            {
                float4 _766 = _695 - _686;
                float3 _1047 = _766.xyz;
                bool3 _1048 = bool3(_1047.x >= 0.0f.xxx.x, _1047.y >= 0.0f.xxx.y, _1047.z >= 0.0f.xxx.z);
                precise float4 _1056 = _766 * float4(float3(_1048.x ? _454.x : _452.x, _1048.y ? _454.y : _452.y, _1048.z ? _454.z : _452.z), 1.0f);
                precise float _1061 = _1056.x + _1056.y;
                precise float _1065 = _1061 + _1056.z;
                precise float _1069 = _1065 + _1056.w;
                bool _1072 = !isinf(_1069);
                bool _1079;
                if (_1072)
                {
                    _1079 = _1069 < (-(_659.w + _659.z));
                }
                else
                {
                    _1079 = _1072;
                }
                _776 = _1079;
            }
            else
            {
                _776 = _761;
            }
            _1138 = !_776;
            break;
        } while(false);
        _1139 = uint(_1138);
    }
    else
    {
        _1139 = 0u;
    }
    _462.Store(gl_GlobalInvocationID.x * 4 + 0, _1139);
}

[numthreads(64, 1, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
