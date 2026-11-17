Texture2D<float4> _112 : register(t0, space0);
SamplerState _116 : register(s1, space0);

static float4 gl_FragCoord;
static float4 _138;

struct SPIRV_Cross_Input
{
    float4 gl_FragCoord : SV_Position;
};

struct SPIRV_Cross_Output
{
    float4 _138 : SV_Target0;
};

uint2 spvTextureSize(Texture2D<float4> Tex, uint Level, out uint Param)
{
    uint2 ret;
    Tex.GetDimensions(Level, ret.x, ret.y, Param);
    return ret;
}

void frag_main()
{
    uint _124_dummy_parameter;
    float3 _141 = max(_112.SampleLevel(_116, gl_FragCoord.xy / float2(int2(spvTextureSize(_112, uint(0), _124_dummy_parameter))), 0.0f).xyz, 0.0f.xxx) * 2.0f;
    float3 _246;
    do
    {
        float _178 = min(_141.x, min(_141.y, _141.z));
        float _244;
        if (_178 < 0.07999999821186065673828125f)
        {
            _244 = _178 - ((6.25f * _178) * _178);
        }
        else
        {
            _244 = 0.039999999105930328369140625f;
        }
        float3 _194 = _141 - _244.xxx;
        float _202 = max(_194.x, max(_194.y, _194.z));
        if (_202 < 0.7599999904632568359375f)
        {
            _246 = _194;
            break;
        }
        float _216 = 1.0f - (0.057599999010562896728515625f / (_202 + (-0.519999980926513671875f)));
        _246 = lerp(_194 * (_216 / max(_202, 9.9999996826552253889678874634872e-21f)), _216.xxx, (1.0f - (1.0f / ((0.1500000059604644775390625f * (_202 - _216)) + 1.0f))).xxx);
        break;
    } while(false);
    _138 = float4(clamp(_246, 0.0f.xxx, 1.0f.xxx), 1.0f);
}

SPIRV_Cross_Output main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    frag_main();
    SPIRV_Cross_Output stage_output;
    stage_output._138 = _138;
    return stage_output;
}
