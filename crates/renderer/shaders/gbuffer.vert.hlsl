#include "raster_vertex.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct GbufferVertex {
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
    BISTRO_LOCATION(5) float2 lightmap_uv : TEXCOORD5;
    BISTRO_LOCATION(6) nointerpolation uint lightmap_triangle : TEXCOORD6;
    float4 clip_position : SV_Position;
};

GbufferVertex main(MeshVertex vertex, uint vertex_id : SV_VertexID) {
    GbufferVertex output;
    output.clip_position = project_vertex(camera.view_projection, vertex.position);
    output.world_normal = vertex.normal;
    output.world_tangent = vertex.tangent;
    output.texcoord = vertex.uv;
    output.lightmap_uv = float2(vertex_id%3u==1u, vertex_id%3u==2u);
    output.lightmap_triangle = vertex_id;
    return output;
}
