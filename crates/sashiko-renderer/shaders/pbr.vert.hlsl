#include "raster_vertex.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct PbrVertex {
    BISTRO_LOCATION(0) float3 world_position : TEXCOORD0;
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
    BISTRO_LOCATION(5) float2 lightmap_uv : TEXCOORD5;
    BISTRO_LOCATION(6) nointerpolation uint lightmap_slot : TEXCOORD6;
    float4 clip_position : SV_Position;
};

#ifdef BISTRO_INSTANCING
struct Placements {
    float4 source_scale;
    float4 origin;
    float4 column_step;
    float4 row_step;
    uint instance_offset;
};

#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<Placements> placements : register(b7, space0);

float3 placed_position(float3 original_position, uint instance_id) {
    uint instance_index = min(
        min(instance_id, 23u) + min(placements.instance_offset, 23u), 23u);
    precise float3 local = (original_position - placements.source_scale.xyz) * placements.source_scale.w;
    precise float3 column = placements.column_step.xyz * float(instance_index % 6u);
    precise float3 row = placements.row_step.xyz * float(instance_index / 6u);
    precise float3 destination = placements.origin.xyz + column + row;
    return destination + local;
}
#endif

#ifdef BISTRO_INSTANCING
PbrVertex main(MeshVertex vertex, uint instance_id : SV_InstanceID) {
    float3 world = placed_position(vertex.position, instance_id);
#else
PbrVertex main(MeshVertex vertex) {
    float3 world = vertex.position;
#endif
    PbrVertex output;
    output.clip_position = project_vertex(camera.view_projection, world);
    output.world_position = world;
    output.world_normal = vertex.normal;
    output.world_tangent = vertex.tangent;
    output.texcoord = vertex.uv;
#ifdef RENDERER_LIGHTMAP
    output.lightmap_uv = vertex.surface_uv.xy;
    output.lightmap_slot = surface_placement.slot + (uint)vertex.surface_uv.z;
#else
    output.lightmap_uv = 0.0;
    output.lightmap_slot = 0u;
#endif
    return output;
}
