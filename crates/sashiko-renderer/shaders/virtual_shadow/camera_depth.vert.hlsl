#include "raster_vertex.hlsli"

cbuffer Projection : register(b0) { row_major float4x4 clip_from_world; };
struct Transform { float4 model[3]; uint mirrored; };
#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<Transform> placement : register(b7);

struct Vertex {
    BISTRO_LOCATION(0) float2 uv : TEXCOORD0;
    BISTRO_LOCATION(1) nointerpolation uint mirrored : TEXCOORD1;
    float4 clip : SV_Position;
};
Vertex main(BISTRO_LOCATION(0) float3 position : TEXCOORD0, BISTRO_LOCATION(3) float2 uv : TEXCOORD3) {
    float4 local_position = float4(position, 1.0);
    float3 world = float3(dot(local_position, placement.model[0]), dot(local_position, placement.model[1]),
                          dot(local_position, placement.model[2]));
    Vertex v;
    v.uv = uv;
    v.mirrored = placement.mirrored;
    v.clip = project_vertex(clip_from_world, world);
    return v;
}
