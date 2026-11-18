static const uint3 gl_WorkGroupSize = uint3(8u, 8u, 1u);

RWTexture2D<float4> _22 : register(u0, space1);
Texture2D<float4> _35 : register(t0, space0);

static uint3 gl_GlobalInvocationID;
struct SPIRV_Cross_Input
{
    uint3 gl_GlobalInvocationID : SV_DispatchThreadID;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

uint2 spvImageSize(RWTexture2D<float4> Tex, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(ret.x, ret.y);
    Param = 0u;
    return ret;
}

void comp_main()
{
    do
    {
        int2 _17 = int2(gl_GlobalInvocationID.xy);
        uint _24_dummy_parameter;
        int2 _24 = int2(spvImageSize(_22, _24_dummy_parameter));
        if (any(bool2(_17.x >= _24.x, _17.y >= _24.y)))
        {
            break;
        }
        uint _38_dummy_parameter;
        int2 _41 = int2(spvTextureSize(_35, uint(0), _38_dummy_parameter)) - int2(1, 1);
        int _117;
        float3 _118;
        _118 = 0.0f.xxx;
        _117 = -1;
        float3 _121;
        for (; _117 <= 1; _118 = _121, _117++)
        {
            _121 = _118;
            for (int _119 = -1; _119 <= 1; )
            {
                _121 += (_35.Load(int3(clamp(_17 + int2(_119, _117), int2(0, 0), _41), 0)).xyz * float((2 - abs(_119)) * (2 - abs(_117))));
                _119++;
                continue;
            }
        }
        _22[_17] = float4(_118 * 0.0625f, 0.0f);
        break;
    } while(false);
}

[numthreads(8, 8, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
