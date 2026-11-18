#version 450 core

layout(location = 0) in vec2 position;
layout(location = 0) out vec3 vertex_color;

layout(set = 0, binding = 2, std430) readonly buffer Instances {
    vec4 shift_color[2];
} instances;

layout(push_constant) uniform Draw {
    uint instance_offset;
} draw;

void main() {
    uint instance_index = min(min(uint(gl_InstanceIndex), 1u) + min(draw.instance_offset, 1u), 1u);
    vec4 entry = instances.shift_color[instance_index];
    gl_Position = vec4(position + vec2(entry.x, 0.0), 0.5, 1.0);
    vertex_color = entry.yzw;
}
