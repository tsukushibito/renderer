static const uint3 gl_WorkGroupSize = uint3(1u, 1u, 1u);

RWByteAddressBuffer _10 : register(u0, space1);
ByteAddressBuffer _15 : register(t0, space0);

void comp_main()
{
    _10.Store4(0, _15.Load4(0));
}

[numthreads(1, 1, 1)]
void main()
{
    comp_main();
}
