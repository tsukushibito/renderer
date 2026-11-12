#version 450 core

// One fullscreen triangle; no vertex buffers or interpolated interface.
void main() {
    vec2 position = vec2(float((gl_VertexIndex << 1) & 2),
                         float(gl_VertexIndex & 2));
    gl_Position = vec4(position * 2.0 - 1.0, 0.0, 1.0);
}
