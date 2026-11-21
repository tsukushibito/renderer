static const uint3 gl_WorkGroupSize = uint3(1u, 1u, 1u);

RWByteAddressBuffer _10 : register(u0, space1);
cbuffer _13_15 : register(b0, space0)
{
    uint4 _15_m0 : packoffset(c0);
};


void comp_main()
{
    _10.Store4(0, _15_m0);
}

[numthreads(1, 1, 1)]
void main()
{
    comp_main();
}
