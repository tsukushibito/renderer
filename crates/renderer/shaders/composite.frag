#version 450 core

layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 output_color;

void main() {
    // Equal source/target extents, positive viewport height, pixel-center fetch.
    // UNORM stores linear values; no sRGB decode/encode or filtering is used.
    output_color = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
}
