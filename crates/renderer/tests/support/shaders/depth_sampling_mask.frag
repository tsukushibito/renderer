#version 450 core

layout(set = 0, binding = 0) uniform texture2D alpha_image;
layout(set = 0, binding = 1) uniform sampler alpha_sampler;

void main() {
    // No color/depth outputs: discarded fragments must not write depth.
    if (textureLod(sampler2D(alpha_image, alpha_sampler), gl_FragCoord.xy / 4.0, 0.0).a < 0.5) {
        discard;
    }
}
