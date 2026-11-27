// One bit per virtual 128x128 page. Each level covers 128x128 pages.
#include "grid.hlsli"
Texture2D<float> depth_image : register(t1);
RWStructuredBuffer<uint> requested : register(u2);

void request_page(uint level, int2 page) {
    if (any(page < 0) || any(page >= 128)) return;
    uint address = level * 16384 + uint(page.y) * 128 + uint(page.x);
    InterlockedOr(requested[address >> 5], 1u << (address & 31));
}

void mark_receiver(float3 position) {
    float distance = length(position - eye_distance.xyz);
    if (!(distance <= eye_distance.w)) return;
    float2 light_uv = float2(dot(light_x.xyz, position), dot(light_y.xyz, position));
    // Choose the finest valid level for this receiver. The reduced footprint
    // at distance preserves the finite pool. Ancestors can service a miss.
    for (uint level = virtual_receiver_level(distance); level < 8; ++level) {
        float2 texel = virtual_page_texel(light_uv, level, page_origin[level].xy);
        if (!all(texel >= 0.0) || !all(texel < 16384.0)) continue;
        // The normal bias may move the centre onto a different page.
        // All PCF taps live in the centre page and its rasterized gutter.
        float radius = shadow_bias.z;
        int2 lo = int2(floor(max(texel - radius, 0.0) / 128.0));
        int2 hi = min(int2(floor(min(texel + radius, 16384.0) / 128.0)), 127);
        if (any(hi - lo > 1)) {
            for (int y = lo.y; y <= hi.y; ++y)
                for (int x = lo.x; x <= hi.x; ++x) request_page(level, int2(x, y));
            continue;
        }
        request_page(level, lo);
        if (hi.x != lo.x) request_page(level, int2(hi.x, lo.y));
        if (hi.y != lo.y) request_page(level, int2(lo.x, hi.y));
        if (all(hi != lo)) request_page(level, hi);
    }
}

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint w, h;
    depth_image.GetDimensions(w, h);
    // Fixed 32x32 groups. Each pixel belongs to exactly one invocation.
    for (uint y = id.y; y < h; y += 256) {
        for (uint x = id.x; x < w; x += 256) {
            float depth = depth_image.Load(int3(x, y, 0));
            if (!(depth >= 0.0 && depth < 1.0)) continue;
            float4 world = receiver_world(uint2(x, y), uint2(w, h), depth);
            if (abs(world.w) > 1e-20) mark_receiver(world.xyz / world.w);
        }
    }
}
