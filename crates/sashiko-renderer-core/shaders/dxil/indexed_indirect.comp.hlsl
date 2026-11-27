struct _42
{
    uint _m0;
    uint _m1;
    uint _m2;
    int _m3;
    uint _m4;
};

static const uint3 gl_WorkGroupSize = uint3(64u, 1u, 1u);

ByteAddressBuffer _22 : register(t0, space0);
ByteAddressBuffer _33 : register(t1, space0);
RWByteAddressBuffer _46 : register(u2, space0);

static uint3 gl_GlobalInvocationID;
struct SPIRV_Cross_Input
{
    uint3 gl_GlobalInvocationID : SV_DispatchThreadID;
};

void comp_main()
{
    uint4 _28 = _22.Load4(gl_GlobalInvocationID.x * 16 + 0);
    uint _37 = _33.Load(gl_GlobalInvocationID.x * 4 + 0);
    _46.Store(gl_GlobalInvocationID.x * 20 + 0, _28.y);
    _46.Store(gl_GlobalInvocationID.x * 20 + 4, min(_37, _28.z));
    _46.Store(gl_GlobalInvocationID.x * 20 + 8, _28.x);
    _46.Store(gl_GlobalInvocationID.x * 20 + 12, uint(0));
    _46.Store(gl_GlobalInvocationID.x * 20 + 16, 0u);
}

[numthreads(64, 1, 1)]
void main(SPIRV_Cross_Input stage_input)
{
    gl_GlobalInvocationID = stage_input.gl_GlobalInvocationID;
    comp_main();
}
