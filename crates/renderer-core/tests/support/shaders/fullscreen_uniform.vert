#version 450 core
layout(set = 0, binding = 0, std140) uniform Color {
    vec4 rgba;
} source_color;
layout(location = 0) out vec4 vertex_color;

void main() {
    vec2 position = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2));
    gl_Position = vec4(position * 2.0 - 1.0, 0.0, 1.0);
    vertex_color = source_color.rgba;
}
