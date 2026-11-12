#version 450 core

// Packed vertex: xyz f32 at byte 0, rgba f32 at byte 12, stride 28 bytes.
layout(location = 0) in vec3 position;
layout(location = 1) in vec4 color;
layout(location = 0) out vec4 vertex_color;

// Column-major clip_from_model at byte 0, linear tint at byte 64: 80 bytes.
layout(push_constant) uniform DrawParameters {
    mat4 clip_from_model;
    vec4 tint;
} parameters;

void main() {
    gl_Position = parameters.clip_from_model * vec4(position, 1.0);
    vertex_color = color * parameters.tint;
}
