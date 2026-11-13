static const uint3 gl_WorkGroupSize = uint3(8u, 8u, 1u);

cbuffer _42_44
{
    uint _44_m0 : packoffset(c0);
};

RWTexture2D<unorm float4> _22 : register(u1, space0);
RWTexture2D<unorm float4> _35 : register(u0, space0);

static uint3 gl_GlobalInvocationID;
struct SPIRV_Cross_Input
{
    uint3 gl_GlobalInvocationID : SV_DispatchThreadID;
};

uint2 spvImageSize(RWTexture2D<unorm float4> Tex, out uint Param)
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
        float4 _38 = _35[_17];
        float3 _84;
        if (_44_m0 == 0u)
        {
            _84 = 1.0f.xxx - _38.xyz;
        }
        else
        {
            _84 = _38.zyx;
        }
        _22[_17] = float4(_84, _38.w);
        break;
    } while(false);
}

[numthreads(8, 8, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
