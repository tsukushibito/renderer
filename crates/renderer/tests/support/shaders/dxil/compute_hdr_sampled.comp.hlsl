static const uint3 gl_WorkGroupSize = uint3(4u, 4u, 1u);

cbuffer _64_66 : register(b2, space0)
{
    float4 _66_m0 : packoffset(c0);
};

RWTexture2D<float4> _22 : register(u0, space1);
Texture2D<float4> _50 : register(t0, space0);
SamplerState _54 : register(s1, space0);

static uint3 gl_GlobalInvocationID;
struct SPIRV_Cross_Input
{
    uint3 gl_GlobalInvocationID : SV_DispatchThreadID;
};

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
        _22[_17] = _50.SampleLevel(_54, (float2(_17) + 0.5f.xx) / float2(_24), 0.0f) * _66_m0;
        break;
    } while(false);
}

[numthreads(4, 4, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
