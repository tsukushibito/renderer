struct _42
{
    float4 _m0;
    float4 _m1;
};

static const uint3 gl_WorkGroupSize = uint3(32u, 1u, 1u);

ByteAddressBuffer _47 : register(t2, space0);
cbuffer _195_197 : register(b0, space0)
{
    row_major float4x4 _197_m0 : packoffset(c0);
};

RWByteAddressBuffer _352 : register(u3, space0);

static uint3 gl_GlobalInvocationID;
struct SPIRV_Cross_Input
{
    uint3 gl_GlobalInvocationID : SV_DispatchThreadID;
};

void comp_main()
{
    uint _140 = gl_GlobalInvocationID.x * 65u;
    uint2 _390 = clamp(_47.Load4(0).xy, uint2(1u, 1u), uint2(65535u, 65535u));
    uint _397 = (_390.x + 15u) / 16u;
    uint2 _158 = uint2(gl_GlobalInvocationID.x % _397, gl_GlobalInvocationID.x / _397) * uint2(16u, 16u);
    uint _685;
    if (all(bool2(_158.x < _390.x, _158.y < _390.y)))
    {
        float2 _181 = float2(_390);
        float2 _184 = ((float2(_158) * 2.0f) / _181) - 1.0f.xx;
        float2 _193 = ((float2(min((_158 + uint2(16u, 16u)), _390)) * 2.0f) / _181) - 1.0f.xx;
        float4 _412 = float4(_197_m0[0].x, _197_m0[1].x, _197_m0[2].x, _197_m0[3].x);
        float4 _427 = float4(_197_m0[0].y, _197_m0[1].y, _197_m0[2].y, _197_m0[3].y);
        float4 _442 = float4(_197_m0[0].z, _197_m0[1].z, _197_m0[2].z, _197_m0[3].z);
        float4 _457 = float4(_197_m0[0].w, _197_m0[1].w, _197_m0[2].w, _197_m0[3].w);
        float4 _228 = _412 - (_457 * _184.x);
        float4 _235 = (_457 * _193.x) - _412;
        float4 _242 = _427 - (_457 * _184.y);
        float4 _249 = (_457 * _193.y) - _427;
        float4 _253 = _457 - _442;
        uint _462 = min(_47.Load(8), 64u);
        uint _686;
        _686 = 0u;
        uint _702;
        for (uint _684 = 0u; _684 < _462; _686 = _702, _684++)
        {
            bool _274 = asfloat(_47.Load4(_684 * 32 + 16)).w > 0.0f;
            bool _286;
            if (_274)
            {
                float4 _475 = _228 * float4(asfloat(_47.Load4(_684 * 32 + 16)).xyz, 1.0f);
                float _480 = asfloat(_47.Load4(_684 * 32 + 16)).w * length(_228.xyz);
                _286 = !(dot(_475, 1.0f.xxxx) < ((-_480) - (((dot(abs(_475), 1.0f.xxxx) + _480) + 1.0f) * 9.9999997473787516355514526367188e-06f)));
            }
            else
            {
                _286 = _274;
            }
            bool _298;
            if (_286)
            {
                float4 _507 = _235 * float4(asfloat(_47.Load4(_684 * 32 + 16)).xyz, 1.0f);
                float _512 = asfloat(_47.Load4(_684 * 32 + 16)).w * length(_235.xyz);
                _298 = !(dot(_507, 1.0f.xxxx) < ((-_512) - (((dot(abs(_507), 1.0f.xxxx) + _512) + 1.0f) * 9.9999997473787516355514526367188e-06f)));
            }
            else
            {
                _298 = _286;
            }
            bool _310;
            if (_298)
            {
                float4 _539 = _242 * float4(asfloat(_47.Load4(_684 * 32 + 16)).xyz, 1.0f);
                float _544 = asfloat(_47.Load4(_684 * 32 + 16)).w * length(_242.xyz);
                _310 = !(dot(_539, 1.0f.xxxx) < ((-_544) - (((dot(abs(_539), 1.0f.xxxx) + _544) + 1.0f) * 9.9999997473787516355514526367188e-06f)));
            }
            else
            {
                _310 = _298;
            }
            bool _322;
            if (_310)
            {
                float4 _571 = _249 * float4(asfloat(_47.Load4(_684 * 32 + 16)).xyz, 1.0f);
                float _576 = asfloat(_47.Load4(_684 * 32 + 16)).w * length(_249.xyz);
                _322 = !(dot(_571, 1.0f.xxxx) < ((-_576) - (((dot(abs(_571), 1.0f.xxxx) + _576) + 1.0f) * 9.9999997473787516355514526367188e-06f)));
            }
            else
            {
                _322 = _310;
            }
            bool _334;
            if (_322)
            {
                float4 _603 = _442 * float4(asfloat(_47.Load4(_684 * 32 + 16)).xyz, 1.0f);
                float _608 = asfloat(_47.Load4(_684 * 32 + 16)).w * length(_442.xyz);
                _334 = !(dot(_603, 1.0f.xxxx) < ((-_608) - (((dot(abs(_603), 1.0f.xxxx) + _608) + 1.0f) * 9.9999997473787516355514526367188e-06f)));
            }
            else
            {
                _334 = _322;
            }
            bool _346;
            if (_334)
            {
                float4 _635 = _253 * float4(asfloat(_47.Load4(_684 * 32 + 16)).xyz, 1.0f);
                float _640 = asfloat(_47.Load4(_684 * 32 + 16)).w * length(_253.xyz);
                _346 = !(dot(_635, 1.0f.xxxx) < ((-_640) - (((dot(abs(_635), 1.0f.xxxx) + _640) + 1.0f) * 9.9999997473787516355514526367188e-06f)));
            }
            else
            {
                _346 = _334;
            }
            if (_346)
            {
                _352.Store(((_140 + 1u) + _686) * 4 + 0, _684);
                _702 = _686 + uint(1);
            }
            else
            {
                _702 = _686;
            }
        }
        _685 = _686;
    }
    else
    {
        _685 = 0u;
    }
    for (uint _687 = _685; _687 < 64u; )
    {
        _352.Store(((_140 + 1u) + _687) * 4 + 0, 0u);
        _687++;
        continue;
    }
    _352.Store(_140 * 4 + 0, _685);
}

[numthreads(32, 1, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
