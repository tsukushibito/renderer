#version 450 core
// Adapted from Khronos PBR Neutral, commit
// 180b1a7bddec33f73fe41712a2963cc3ad8e5547. Apache-2.0; see licenses/.
// Changes: explicit nonnegative input, fixed EV +1 and separate sRGB output.
layout(set = 0, binding = 0) uniform texture2D linear_image;
layout(set = 0, binding = 1) uniform sampler source_sampler;
layout(location = 0) out vec4 output_color;
vec3 pbr_neutral(vec3 color) {
    const float start_compression = 0.8 - 0.04;
    const float desaturation = 0.15;
    float x = min(color.r, min(color.g, color.b));
    float offset = x < 0.08 ? x - 6.25 * x * x : 0.04;
    color -= offset;
    float peak = max(color.r, max(color.g, color.b));
    if (peak < start_compression) { return color; }
    float d = 1.0 - start_compression;
    float new_peak = 1.0 - d * d / (peak + d - start_compression);
    color *= new_peak / max(peak, 1e-20);
    float g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(color, vec3(new_peak), g);
}
void main() {
    vec2 uv = gl_FragCoord.xy / vec2(textureSize(sampler2D(linear_image, source_sampler), 0));
    vec3 hdr = max(textureLod(sampler2D(linear_image, source_sampler), uv, 0.0).rgb, vec3(0.0));
    output_color = vec4(clamp(pbr_neutral(hdr * 2.0), 0.0, 1.0), 1.0);
}
