#version 450 core

layout(push_constant) uniform InlineColor {
    vec4 rgba;
} color;

layout(location = 0) out vec4 first;
layout(location = 1) out vec4 second;

void main() {
    first = color.rgba;
    second = color.rgba.brga;
}
