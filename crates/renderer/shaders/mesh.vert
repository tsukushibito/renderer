#version 450 core

// Packed vertex: xyz f32 at byte 0, rgba f32 at byte 12, stride 28 bytes.
layout(location = 0) in vec3 position;
layout(location = 1) in vec4 color;
layout(location = 0) out vec4 vertex_color;

void main() {
    gl_Position = vec4(position, 1.0);
    vertex_color = color;
}
