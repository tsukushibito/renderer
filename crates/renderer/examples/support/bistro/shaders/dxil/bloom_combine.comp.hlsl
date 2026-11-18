static const uint3 gl_WorkGroupSize = uint3(8u, 8u, 1u);

Texture2D<float4> _48 : register(t1, space0);
RWTexture2D<float4> _108 : register(u0, space1);
Texture2D<float4> _138 : register(t0, space0);

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
        int2 _104 = int2(gl_GlobalInvocationID.xy);
        uint _110_dummy_parameter;
        int2 _110 = int2(spvImageSize(_108, _110_dummy_parameter));
        if (any(bool2(_104.x >= _110.x, _104.y >= _110.y)))
        {
            break;
        }
        uint _122_dummy_parameter;
        int2 _122 = int2(spvTextureSize(_48, uint(0), _122_dummy_parameter));
        uint _140_dummy_parameter;
        float4 _149 = _138.Load(int3(min(_104, (int2(spvTextureSize(_138, uint(0), _140_dummy_parameter)) - int2(1, 1))), 0));
        int2 _193 = _122 - int2(1, 1);
        float2 _195 = clamp(((float2(_104) + 0.5f.xx) * (float2(_122) / float2(_110))) - 0.5f.xx, 0.0f.xx, float2(_193));
        int2 _197 = int2(_195);
        int2 _204 = min((_197 + int2(1, 1)), _193);
        float2 _208 = _195 - float2(_197);
        float3 _223 = _208.x.xxx;
        _108[_104] = float4(min(_149.xyz + (lerp(lerp(_48.Load(int3(_197, 0)).xyz, _48.Load(int3(int2(_204.x, _197.y), 0)).xyz, _223), lerp(_48.Load(int3(int2(_197.x, _204.y), 0)).xyz, _48.Load(int3(_204, 0)).xyz, _223), _208.y.xxx) * 0.125f), 65504.0f.xxx), _149.w);
        break;
    } while(false);
}

[numthreads(8, 8, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
