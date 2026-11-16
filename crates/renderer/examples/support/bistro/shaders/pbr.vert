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
    vec4 ambient;
} camera;

void main() {
    gl_Position = camera.view_projection * vec4(position, 1.0);
    world_position = position;
    world_normal = normal;
    world_tangent = tangent;
    texcoord = uv;
}
