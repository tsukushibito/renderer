StructuredBuffer<uint> lists : register(t0);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint> counts : register(u1);

[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    counts[id.x] = lists[id.x * 1024];
}
