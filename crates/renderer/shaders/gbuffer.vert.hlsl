#include "raster_vertex.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct GbufferVertex {
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
    BISTRO_LOCATION(5) float2 lightmap_uv : TEXCOORD5;
    BISTRO_LOCATION(6) nointerpolation uint lightmap_slot : TEXCOORD6;
    float4 clip_position : SV_Position;
};

GbufferVertex main(MeshVertex vertex) {
    GbufferVertex output;
    output.clip_position = project_vertex(camera.view_projection, vertex.position);
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
