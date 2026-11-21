cbuffer _140_142 : register(b0, space0)
{
    float _142_m0 : packoffset(c0);
};

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
    float3 _146 = max(_112.SampleLevel(_116, gl_FragCoord.xy / float2(int2(spvTextureSize(_112, uint(0), _124_dummy_parameter))), 0.0f).xyz, 0.0f.xxx) * _142_m0;
    float3 _251;
    do
    {
        float _183 = min(_146.x, min(_146.y, _146.z));
        float _249;
        if (_183 < 0.07999999821186065673828125f)
        {
            _249 = _183 - ((6.25f * _183) * _183);
        }
        else
        {
            _249 = 0.039999999105930328369140625f;
        }
        float3 _199 = _146 - _249.xxx;
        float _207 = max(_199.x, max(_199.y, _199.z));
        if (_207 < 0.7599999904632568359375f)
        {
            _251 = _199;
            break;
        }
        float _221 = 1.0f - (0.057599999010562896728515625f / (_207 + (-0.519999980926513671875f)));
        _251 = lerp(_199 * (_221 / max(_207, 9.9999996826552253889678874634872e-21f)), _221.xxx, (1.0f - (1.0f / ((0.1500000059604644775390625f * (_207 - _221)) + 1.0f))).xxx);
        break;
    } while(false);
    _138 = float4(clamp(_251, 0.0f.xxx, 1.0f.xxx), 1.0f);
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
