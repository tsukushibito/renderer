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
    // World-space geometry close to the camera can cancel large MVP terms.
    // Use one shared FP32 operation order for both native compilers instead of
    // permitting each matrix-dot lowering to reassociate/contract differently.
    // This is a reproducibility boundary, not a higher-precision coordinate ABI.
    precise vec4 x = camera.view_projection[0] * position.x;
    precise vec4 y = camera.view_projection[1] * position.y;
    precise vec4 z = camera.view_projection[2] * position.z;
    precise vec4 xy = x + y;
    precise vec4 xyz = xy + z;
    precise vec4 clip = xyz + camera.view_projection[3];
    gl_Position = clip;
    world_position = position;
    world_normal = normal;
    world_tangent = tangent;
    texcoord = uv;
}
