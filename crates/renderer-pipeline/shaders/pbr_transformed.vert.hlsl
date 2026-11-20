#include "raster_vertex.hlsli"
#include "transforms.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct Parameters {
    uint first_transform;
};

#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<Parameters> parameters : register(b7, space0);

struct PbrVertex {
    BISTRO_LOCATION(0) float3 world_position : TEXCOORD0;
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
    float4 clip_position : SV_Position;
};

PbrVertex main(MeshVertex vertex, uint instance_id : SV_InstanceID) {
    uint address = transform_address(parameters.first_transform, instance_id);
    TransformRows model = load_transform_rows(address);
    TransformRows normal = load_transform_rows(address + 48u);
    float3 world = transform_position(model, vertex.position);

    PbrVertex output;
    output.clip_position = project_vertex(camera.view_projection, world);
    output.world_position = world;
    output.world_normal = transform_linear(normal, vertex.normal);
    // The shared fragment material pass orthogonalizes the interpolated
    // tangent against the normal and uses the original handedness.
    output.world_tangent = float4(transform_linear(model, vertex.tangent.xyz), vertex.tangent.w);
    output.texcoord = vertex.uv;
    return output;
}
