#include "raster_vertex.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct GbufferVertex {
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
    float4 clip_position : SV_Position;
};

GbufferVertex main(MeshVertex vertex) {
    GbufferVertex output;
    output.clip_position = project_vertex(camera.view_projection, vertex.position);
    output.world_normal = vertex.normal;
    output.world_tangent = vertex.tangent;
    output.texcoord = vertex.uv;
    return output;
}
