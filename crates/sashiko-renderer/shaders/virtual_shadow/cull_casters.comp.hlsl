#include "grid.hlsli"

StructuredBuffer<uint4> owners : register(t1);
// One rectangle (minX, minY, exclusive maxX, exclusive maxY) per clip level.
StructuredBuffer<uint4> rectangles : register(t2);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint> lists : register(u3);
groupshared uint count;

// One group owns a 4 KiB list. Index zero holds the GPU instance count.
// Compact only dirty intersecting pages; stationary casters can issue zero
// instances without launching any vertex threads.
[numthreads(64, 1, 1)]
void main(uint3 group : SV_GroupID, uint lane : SV_GroupIndex) {
    uint base = group.x * 1024;
    if (lane == 0) count = 0;
    GroupMemoryBarrierWithGroupSync();
    uint mask = 0;
    for (uint bit = 0; bit < 16; ++bit) {
        uint slot = lane * 16 + bit;
        if (slot >= VSM_CAPACITY) break;
        uint4 owner = owners[slot];
        if (owner.w == 0 || owner.x == 0 || owner.x > 8) continue;
        uint level = owner.x - 1;
        uint2 page = owner.yz - asuint(page_origin[level].xy);
        uint4 rect = rectangles[group.x * 8 + level];
        if (all(page >= rect.xy) && all(page < rect.zw)) mask |= (1u << bit);
    }
    uint start = 0;
    if (mask != 0) InterlockedAdd(count, countbits(mask), start);
    GroupMemoryBarrierWithGroupSync();
    if (lane == 0) lists[base] = count;
    while (mask != 0) {
        uint bit = firstbitlow(mask);
        lists[base + 1 + start++] = lane * 16 + bit;
        mask &= mask - 1;
    }
    for (uint i = 1 + count + lane; i < 1024; i += 64) lists[base + i] = 0;
}
