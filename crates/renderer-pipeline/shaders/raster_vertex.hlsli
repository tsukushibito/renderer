#ifndef BISTRO_RASTER_VERTEX_HLSLI
#define BISTRO_RASTER_VERTEX_HLSLI

#include "raster_abi.hlsli"

// Cooked world-space stream. The cook resolves transforms and mirrored
// orientation before draw submission.
struct MeshVertex {
    BISTRO_LOCATION(0) float3 position : TEXCOORD0;
    BISTRO_LOCATION(1) float3 normal : TEXCOORD1;
    BISTRO_LOCATION(2) float4 tangent : TEXCOORD2;
    BISTRO_LOCATION(3) float2 uv : TEXCOORD3;
};

float4 project_vertex(float4x4 matrix_columns, float3 world) {
    // Nearby world-space geometry can cancel large MVP terms. Use the
    // same separate FP32 products and additions for clip and depth in
    // both APIs, including the Forward and G-buffer paths.
    precise float4 x = matrix_columns[0] * world.x;
    precise float4 y = matrix_columns[1] * world.y;
    precise float4 z = matrix_columns[2] * world.z;
    precise float4 xy = x + y;
    precise float4 xyz = xy + z;
    precise float4 clip = xyz + matrix_columns[3];

    // CPU projection matrices store Vulkan clip Y. D3D's viewport
    // convention requires the opposite sign in the vertex output.
#ifndef __spirv__
    clip.y = -clip.y;
#endif
    return clip;
}

#endif
