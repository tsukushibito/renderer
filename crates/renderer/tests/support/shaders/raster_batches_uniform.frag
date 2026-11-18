#version 450 core

layout(set = 0, binding = 2, std140) uniform UniformColor {
    vec4 rgba;
} color;

layout(location = 0) out vec4 first;
layout(location = 1) out vec4 second;

void main() {
    first = color.rgba;
    second = color.rgba.brga;
}
