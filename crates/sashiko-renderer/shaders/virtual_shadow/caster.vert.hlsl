#include "raster_abi.hlsli"
#include "grid.hlsli"

StructuredBuffer<uint4> slots : register(t2);
StructuredBuffer<uint> caster_list : register(t3);
struct Placement {
    float4 model[3];
    uint mirrored;
};
#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<Placement> placement : register(b7);

struct Vertex {
    BISTRO_LOCATION(0) float2 uv : TEXCOORD0;
    BISTRO_LOCATION(1) nointerpolation uint tile : TEXCOORD1;
    BISTRO_LOCATION(2) nointerpolation uint mirrored : TEXCOORD2;
    float4 clip : SV_Position;
};

Vertex main(BISTRO_LOCATION(0) float3 pos : TEXCOORD0,
            BISTRO_LOCATION(3) float2 uv : TEXCOORD3, uint instance : SV_InstanceID) {
    Vertex v;
    v.uv = uv;
    v.tile = 0;
    v.mirrored = placement.mirrored;
    v.clip = float4(2.0, 2.0, 1.0, 1.0);
    if (instance >= VSM_CAPACITY) return v;
    uint slot = caster_list[1 + instance];
    if (slot >= VSM_CAPACITY) return v;
    uint4 owner = slots[slot];
    if (owner.x == 0 || owner.x > 8 || owner.w == 0) return v;
    uint level = owner.x - 1;
    float4 local_position = float4(pos, 1.0);
    float3 world = float3(dot(local_position, placement.model[0]), dot(local_position, placement.model[1]),
                          dot(local_position, placement.model[2]));
    float2 light_uv = float2(dot(light_x.xyz, world), dot(light_y.xyz, world));
    float2 local_uv = light_uv * (25.0 / float(1u << level)) - float2(asint(owner.yz));
    float2 xy = (virtual_tile_origin(slot) + VSM_GUTTER + local_uv * 128.0) / 2048.0 - 1.0;
#ifndef __spirv__
    xy.y = -xy.y;
#endif
    v.tile = slot;
    v.clip = float4(xy, dot(float4(world, 1.0), depth_plane), 1.0);
    return v;
}
