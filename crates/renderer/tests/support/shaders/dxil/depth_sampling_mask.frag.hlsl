Texture2D<float4> _9 : register(t0, space0);
SamplerState _13 : register(s1, space0);

static float4 gl_FragCoord;
struct SPIRV_Cross_Input
{
    float4 gl_FragCoord : SV_Position;
};

void frag_main()
{
    if (_9.SampleLevel(_13, gl_FragCoord.xy * 0.25f.xx, 0.0f).w < 0.5f)
    {
        discard;
    }
}

void main(SPIRV_Cross_Input stage_input)
{
    gl_FragCoord = stage_input.gl_FragCoord;
    gl_FragCoord.w = 1.0 / gl_FragCoord.w;
    frag_main();
}
