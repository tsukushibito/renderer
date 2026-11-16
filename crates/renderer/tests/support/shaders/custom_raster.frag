#version 450 core

layout(location = 0) in vec4 vertex_color;
layout(location = 0) out vec4 output_color;

layout(push_constant) uniform DrawParameters {
    vec4 position;
    vec4 tint;
} parameters;
layout(set = 0, binding = 2, std140) uniform Factor {
    vec4 value;
} factor;
layout(set = 1, binding = 3, std430) readonly buffer Offset {
    vec4 value;
} offset;
layout(set = 2, binding = 4) uniform texture2D source_image;
layout(set = 2, binding = 5) uniform sampler source_sampler;

void main() {
    // Read precisely the declared 16-byte prefixes. Explicit LOD 0 plus the
    // bound NearestClamp sampler stays within the selected single-mip View.
    output_color = (vertex_color * parameters.tint * factor.value + offset.value)
        * textureLod(sampler2D(source_image, source_sampler), vec2(0.5), 0.0);
}
