#version 450 core

// Cooked world-space stream. All transforms, including mirrored orientation,
// are resolved at cook/import boundaries; no model matrix is fetched per draw.
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 normal;
layout(location = 2) in vec4 tangent;
layout(location = 3) in vec2 uv;
layout(location = 0) out vec3 world_position;
layout(location = 1) out vec3 world_normal;
layout(location = 2) out vec4 world_tangent;
layout(location = 3) out vec2 texcoord;
layout(set = 0, binding = 0, std140) uniform Camera {
    mat4 view_projection;
    vec4 eye;
    vec4 light_direction;
    vec4 light_color;
    vec4 environment; // strength, inverse-yaw input radians, specular max LOD, shadows enabled
    mat4 shadow_view_projection[4];
    vec4 shadow_splits;
    vec4 shadow_texel_world;
    vec4 shadow_depth_scale;
    vec4 camera_forward_blend;
    vec4 shadow_bias; // receiver constant/slope/normal offset in world-texel units
} camera;

#ifdef BISTRO_INSTANCING
layout(push_constant) uniform Placements {
    vec4 source_scale;
    vec4 origin;
    vec4 column_step;
    vec4 row_step;
    uint instance_offset;
} placements;

vec3 placed_position(vec3 original_position) {
    uint instance_index = min(
        min(uint(gl_InstanceIndex), 23u) + min(placements.instance_offset, 23u), 23u);
    precise vec3 local = (original_position - placements.source_scale.xyz) * placements.source_scale.w;
    precise vec3 column = placements.column_step.xyz * float(instance_index % 6u);
    precise vec3 row = placements.row_step.xyz * float(instance_index / 6u);
    precise vec3 destination = placements.origin.xyz + column + row;
    return destination + local;
}
#endif

void main() {
#ifdef BISTRO_INSTANCING
    vec3 world = placed_position(position);
#define PBR_POSITION world
#else
#define PBR_POSITION position
#endif
    // World-space geometry close to the camera can cancel large MVP terms.
    // Use one shared FP32 operation order for both native compilers instead of
    // permitting each matrix-dot lowering to reassociate/contract differently.
    // This is a reproducibility boundary, not a higher-precision coordinate ABI.
    precise vec4 x = camera.view_projection[0] * PBR_POSITION.x;
    precise vec4 y = camera.view_projection[1] * PBR_POSITION.y;
    precise vec4 z = camera.view_projection[2] * PBR_POSITION.z;
    precise vec4 xy = x + y;
    precise vec4 xyz = xy + z;
    precise vec4 clip = xyz + camera.view_projection[3];
    gl_Position = clip;
    world_position = PBR_POSITION;
    world_normal = normal;
    world_tangent = tangent;
    texcoord = uv;
}
