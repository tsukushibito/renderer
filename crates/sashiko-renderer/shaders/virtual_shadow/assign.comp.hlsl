#include "virtual_shadow_math.hlsli"
StructuredBuffer<uint> requests : register(t0);
StructuredBuffer<uint> counts : register(t1);
StructuredBuffer<uint2> mapping : register(t3);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint> table : register(u2);

// Each lane writes all 32 entries for one request word, including misses.
// Zero means unmapped; the other entries encode physical slot + 1.
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint order = id.x / 512;
    uint level = (order + 7u) & 7u;
    uint word = level * 512 + id.x % 512;
    uint block = id.x / 64;
    uint rank = 0;
    for (uint b = 0; b < block; ++b) rank += min(counts[b], 2048u);
    for (uint w = word & ~63u; w < word; ++w) rank += countbits(requests[w]);
    uint mask = requests[word];
    [unroll]
    for (uint bit = 0; bit < 32; ++bit) {
        bool hit = (mask & (1u << bit)) != 0;
        table[word * 32 + bit] = hit && rank < VSM_CAPACITY ? min(mapping[min(rank, VSM_CAPACITY - 1u)].x, VSM_CAPACITY - 1u) + 1u : 0u;
        if (hit) ++rank;
    }
}
