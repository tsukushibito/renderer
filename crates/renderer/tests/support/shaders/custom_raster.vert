#version 450 core

// Consumer-owned input ABI: xy at byte 0, rgba at byte 8, stride 24.
layout(location = 0) in vec2 position;
layout(location = 1) in vec4 color;
layout(location = 0) out vec4 vertex_color;

layout(push_constant) uniform DrawParameters {
    vec4 position;
    vec4 tint;
} parameters;

void main() {
    gl_Position = vec4(position + parameters.position.xy, parameters.position.z, 1.0);
    vertex_color = color;
}
