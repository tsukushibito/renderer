StructuredBuffer<uint> requests : register(t0);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint> counts : register(u1);

// One thread owns each block of 64 words. Prioritize level 7 (the coarse
// fallback) and then levels 0..6 so pressure favors the nearest receivers.
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint block = id.x;
    uint level = ((block >> 3) + 7u) & 7u;
    uint first = level * 512 + (block & 7u) * 64;
    uint count = 0;
    for (uint w = 0; w < 64; ++w) count += countbits(requests[first + w]);
    counts[block] = count;
}
