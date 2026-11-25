#include "raster_vertex.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct ShadowParameters {
    uint cascade_index;
};

#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<ShadowParameters> parameters : register(b7, space0);

struct ShadowVertex {
    BISTRO_LOCATION(0) float2 texcoord : TEXCOORD0;
    float4 clip_position : SV_Position;
};

ShadowVertex main(
    BISTRO_LOCATION(0) float3 position : TEXCOORD0,
    BISTRO_LOCATION(3) float2 uv : TEXCOORD3
) {
    uint cascade = min(parameters.cascade_index, 3u);
    ShadowVertex output;
    output.clip_position = project_vertex(camera.shadow_view_projection[cascade], position);
    output.texcoord = uv;
    return output;
}
