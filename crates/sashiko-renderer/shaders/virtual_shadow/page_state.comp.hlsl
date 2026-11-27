#include "grid.hlsli"

StructuredBuffer<uint> requests : register(t1);
StructuredBuffer<uint> counts : register(t2);
StructuredBuffer<uint4> previous : register(t3);
StructuredBuffer<uint2> mapping : register(t5);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint4> slots : register(u4);

// Convert a physical slot's rank into its virtual address. Each invocation
// publishes one complete record even when the slot has no owner. The narrowed
// rank can cross at most 31 set bits in the selected word.
uint4 owner(uint rank) {
    for (uint block = 0; block < 64; ++block) {
        uint count = min(counts[block], 2048u);
        if (rank >= count) {
            rank -= count;
            continue;
        }
        uint level = ((block >> 3) + 7u) & 7u;
        uint first = (block & 7u) * 64;
        for (uint word = first; word < first + 64; ++word) {
            uint mask = requests[level * 512 + word];
            uint n = countbits(mask);
            if (rank >= n) {
                rank -= n;
                continue;
            }
            for (uint i = 0; i < rank; ++i) mask &= mask - 1;
            uint offset = word * 32 + firstbitlow(mask);
            uint2 world = asuint(page_origin[level].xy) + uint2(offset % 128, offset / 128);
            return uint4(level + 1, world, 0);
        }
        break;
    }
    return 0;
}

[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= VSM_CAPACITY) return;
    uint4 current = owner(mapping[id.x].y);
    // Ignore the previous frame's dirty flag. Its depth is valid as soon as
    // the graph containing the corresponding tile raster has been accepted.
    current.w = current.x != 0 && any(current.xyz != previous[id.x].xyz) ? 1u : 0u;
    slots[id.x] = current;
}
