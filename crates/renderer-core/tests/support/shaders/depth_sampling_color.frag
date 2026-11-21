#version 450 core

layout(location = 0) out vec4 output_color;
layout(push_constant) uniform SamplingParameters {
    vec4 settings;
} parameters;
layout(set = 0, binding = 0) uniform texture2D color_image;
layout(set = 0, binding = 1) uniform sampler color_sampler;
layout(set = 0, binding = 2) uniform texture2D depth_image;
layout(set = 0, binding = 3) uniform sampler depth_sampler;

void main() {
    // The UVs include outside edges and fractional texels. LOD 0.5 also
    // exercises mip interpolation while depth is always sampled with nearest.
    vec2 uv = gl_FragCoord.xy * 0.5 - 0.375;
    vec2 color = textureLod(sampler2D(color_image, color_sampler), uv, parameters.settings.x).rg;
    float depth = textureLod(sampler2D(depth_image, depth_sampler), gl_FragCoord.xy / 4.0, 0.0).r;
    output_color = vec4(color, depth, 1.0);
}
