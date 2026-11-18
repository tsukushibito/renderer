#version 450 core
layout(set = 0, binding = 0) uniform texture2D linear_image;
layout(set = 0, binding = 1) uniform sampler source_sampler;
layout(location = 0) out vec4 output_color;
void main() {
    // Equal extents, top-left logical pixels on both APIs; no exposure or tone
    // mapping. Sampling uses the selected mip-zero View and nearest clamp.
    vec2 uv = gl_FragCoord.xy / vec2(textureSize(sampler2D(linear_image, source_sampler), 0));
    vec3 linear_color = clamp(textureLod(sampler2D(linear_image, source_sampler), uv, 0.0).rgb, 0.0, 1.0);
#ifdef RENDERER_LINEAR_OUTPUT
    output_color = vec4(linear_color, 1.0);
#else
    vec3 encoded = mix(12.92 * linear_color,
        1.055 * pow(linear_color, vec3(1.0 / 2.4)) - 0.055,
        greaterThan(linear_color, vec3(0.0031308)));
    output_color = vec4(encoded, 1.0);
#endif
}
