#include "raster_vertex.hlsli"
#include "transforms.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct Parameters {
    uint cascade_index;
    uint first_transform;
};

#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<Parameters> parameters : register(b7, space0);

struct ShadowVertex {
    BISTRO_LOCATION(0) float2 texcoord : TEXCOORD0;
    float4 clip_position : SV_Position;
};

ShadowVertex main(
    BISTRO_LOCATION(0) float3 position : TEXCOORD0,
    BISTRO_LOCATION(3) float2 uv : TEXCOORD3,
    uint instance_id : SV_InstanceID
) {
    uint cascade = min(parameters.cascade_index, 3u);
    uint address = transform_address(parameters.first_transform, instance_id);
    float3 world = transform_position(load_transform_rows(address), position);
    ShadowVertex output;
    output.clip_position = project_vertex(camera.shadow_view_projection[cascade], world);
    output.texcoord = uv;
    return output;
}
