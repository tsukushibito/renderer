#version 450 core

// The mesh ABI is unchanged: xyz f32 at 0, rgba f32 at 12, stride 28.
layout(location = 0) in vec3 position;
layout(location = 1) in vec4 color;
layout(location = 0) out vec4 vertex_color;
layout(location = 1) out vec2 sample_uv;

layout(push_constant) uniform DrawParameters {
    mat4 clip_from_model;
    vec4 tint;
} parameters;

// A separate resource read, not an alias of the inline parameter block.
// Every component of this 16-byte prefix participates in the result.
layout(set = 0, binding = 0, std140) uniform VertexOffset {
    vec4 clip_offset;
} vertex_offset;

void main() {
    gl_Position = parameters.clip_from_model * vec4(position, 1.0)
        + vertex_offset.clip_offset;
    vertex_color = color * parameters.tint;
    sample_uv = position.xy * 0.5 + 0.5;
}
