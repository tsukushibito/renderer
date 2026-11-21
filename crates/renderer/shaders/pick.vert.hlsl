#include "raster_vertex.hlsli"

struct PickParameters {
    row_major float4x4 clip_from_world;
    float4 model_rows[3];
    uint id;
};
#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<PickParameters> pick : register(b7, space0);

struct Output {
    BISTRO_LOCATION(0) nointerpolation float4 id_bytes : TEXCOORD0;
    float4 position : SV_Position;
};

Output main(BISTRO_LOCATION(0) float3 position : TEXCOORD0) {
    float4 local = float4(position, 1.0);
    float3 world = float3(dot(pick.model_rows[0], local), dot(pick.model_rows[1], local), dot(pick.model_rows[2], local));
    Output result;
    result.position = project_vertex(pick.clip_from_world, world);
    result.id_bytes = float4((pick.id >> uint4(0, 8, 16, 24)) & 255u) / 255.0;
    return result;
}
