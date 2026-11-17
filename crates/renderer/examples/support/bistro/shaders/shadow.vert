#version 450 core
layout(location = 0) in vec3 position;
layout(location = 3) in vec2 uv;
layout(location = 0) out vec2 texcoord;
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
layout(push_constant) uniform ShadowParameters { uint cascade_index; } parameters;
void main() {
    uint index = min(parameters.cascade_index, 3u);
    mat4 matrix = camera.shadow_view_projection[index];
    precise vec4 x = matrix[0] * position.x;
    precise vec4 y = matrix[1] * position.y;
    precise vec4 z = matrix[2] * position.z;
    precise vec4 xy = x + y;
    precise vec4 xyz = xy + z;
    precise vec4 clip = xyz + matrix[3];
    gl_Position = clip;
    texcoord = uv;
}
