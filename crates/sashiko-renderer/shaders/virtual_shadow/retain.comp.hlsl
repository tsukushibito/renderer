#include "grid.hlsli"

StructuredBuffer<uint> requests : register(t1);
StructuredBuffer<uint> counts : register(t2);
StructuredBuffer<uint4> previous : register(t3);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint2> mapping : register(u4);

// Both permutations include unused ranks, so every output is initialized.
groupshared uint claim[VSM_CAPACITY];
groupshared uint old_rank[VSM_CAPACITY];
groupshared uint missing[VSM_CAPACITY];
groupshared uint available[VSM_CAPACITY];
// Reuse claim and old_rank as the completed permutations after compaction.
groupshared uint2 prefixes[64];

uint requested_rank(uint4 prior) {
    if (prior.x == 0 || prior.x > 8) return VSM_CAPACITY;
    uint level = prior.x - 1;
    uint2 local = prior.yz - asuint(page_origin[level].xy);
    if (any(local >= 128)) return VSM_CAPACITY;
    uint index = local.y * 128 + local.x;
    uint word = level * 512 + index / 32;
    uint bit = 1u << (index % 32);
    uint mask = requests[word];
    if ((mask & bit) == 0) return VSM_CAPACITY;
    uint block = ((level + 1) & 7) * 8 + index / 2048;
    uint rank = countbits(mask & (bit - 1u));
    for (uint b = 0; b < block; ++b) rank += min(counts[b], 2048u);
    for (uint w = word & ~63u; w < word; ++w) rank += countbits(requests[w]);
    return min(rank, VSM_CAPACITY);
}

// A retained world page keeps its physical tile regardless of changes to its
// request rank. Pair unmatched ranks and free slots in ascending order.
[numthreads(64, 1, 1)]
void main(uint lane : SV_GroupIndex) {
    uint first = lane * 16;
    uint end = min(first + 16, VSM_CAPACITY);
    for (uint i = first; i < end; ++i) claim[i] = VSM_CAPACITY;
    GroupMemoryBarrierWithGroupSync();
    for (uint i = first; i < end; ++i) {
        uint rank = requested_rank(previous[i]);
        old_rank[i] = rank;
        if (rank < VSM_CAPACITY) InterlockedMin(claim[rank], i);
    }
    GroupMemoryBarrierWithGroupSync();
    uint2 n = 0;
    uint retained_mask = 0;
    for (uint i = first; i < end; ++i) {
        bool taken = old_rank[i] < VSM_CAPACITY && claim[min(old_rank[i], VSM_CAPACITY - 1u)] == i;
        n += uint2(claim[i] == VSM_CAPACITY, !taken);
        if (taken) retained_mask |= 1u << (i - first);
    }
    prefixes[lane] = n;
    GroupMemoryBarrierWithGroupSync();
    uint2 offset = 0;
    for (uint l = 0; l < lane; ++l) offset += prefixes[l];
    for (uint i = first; i < end; ++i) {
        if (claim[i] == VSM_CAPACITY) missing[offset.x++] = i;
        if (old_rank[i] == VSM_CAPACITY || claim[min(old_rank[i], VSM_CAPACITY - 1u)] != i)
            available[offset.y++] = i;
    }
    GroupMemoryBarrierWithGroupSync();
    for (uint j = offset.x - n.x; j < offset.x; ++j) {
        uint rank = missing[j];
        uint slot = available[j];
        claim[rank] = slot;
        old_rank[slot] = rank;
    }
    GroupMemoryBarrierWithGroupSync();
    for (uint i = first; i < end; ++i) {
        uint retained = (retained_mask >> (i - first)) & 1u;
        mapping[i] = uint2(claim[i], old_rank[i] | (retained << 31));
    }
}
