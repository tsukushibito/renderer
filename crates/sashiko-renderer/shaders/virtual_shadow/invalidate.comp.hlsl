#include "grid.hlsli"
StructuredBuffer<uint> changed : register(t1);
RWStructuredBuffer<uint4> slots : register(u2);
[numthreads(64,1,1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint4 owner = slots[id.x];
    if (owner.x == 0 || owner.x > 8 || owner.w != 0) return;
    uint level = owner.x - 1;
    uint2 page = owner.yz - asuint(page_origin[level].xy);
    if (any(page >= 128)) return;
    uint index = (level * 128 + page.y) * 128 + page.x;
    if ((changed[index >> 5] & (1u << (index % 32))) != 0) slots[id.x].w = 1;
}
