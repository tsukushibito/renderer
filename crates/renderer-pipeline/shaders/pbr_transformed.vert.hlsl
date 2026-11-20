#include "raster_vertex.hlsli"
#include "transforms.hlsli"
#include "direction_scale.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);

struct Parameters {
    uint first_transform;
};

#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<Parameters> parameters : register(b7, space0);

struct PbrVertex {
#ifndef RENDERER_GBUFFER_VERTEX
    BISTRO_LOCATION(0) float3 world_position : TEXCOORD0;
#endif
    BISTRO_LOCATION(1) float3 world_normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 world_tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 texcoord : TEXCOORD3;
    BISTRO_LOCATION(4) nointerpolation uint mirrored : TEXCOORD4;
    float4 clip_position : SV_Position;
};

float3 transform_direction(TransformRows rows, float3 value) {
    uint magnitude = max(max_direction_magnitude(rows.x.xyz),
        max(max_direction_magnitude(rows.y.xyz), max_direction_magnitude(rows.z.xyz)));
    int shift = extreme_direction_shift(magnitude);
    if (shift != 0) {
        // Apply one common factor across rows and vertices so perspective
        // interpolation preserves the original direction field.
        rows.x.xyz = shift_direction(rows.x.xyz, shift);
        rows.y.xyz = shift_direction(rows.y.xyz, shift);
        rows.z.xyz = shift_direction(rows.z.xyz, shift);
    }
    return transform_linear(rows, value);
}

PbrVertex main(MeshVertex vertex, uint instance_id : SV_InstanceID) {
    uint address = transform_address(parameters.first_transform, instance_id);
    TransformRows model = load_transform_rows(address);
    TransformRows normal = load_transform_rows(address + 48u);
    float3 world = transform_position(model, vertex.position);

    PbrVertex output;
    output.clip_position = project_vertex(camera.view_projection, world);
#ifndef RENDERER_GBUFFER_VERTEX
    output.world_position = world;
#endif
    output.world_normal = transform_direction(normal, vertex.normal);
    output.mirrored = transform_is_mirrored(normal.x.w);
    // A reflected basis flips the tangent frame even when the physical
    // front-face classification is restored in the fragment shader.
    float handedness = output.mirrored != 0u ? -vertex.tangent.w : vertex.tangent.w;
    output.world_tangent = float4(transform_direction(model, vertex.tangent.xyz), handedness);
    output.texcoord = vertex.uv;
    return output;
}
