#version 450 core
layout(location = 0) in vec2 texcoord;
layout(set = 0, binding = 1, std140) uniform Material {
    vec4 base_factor;
    vec4 emissive_normal_scale;
    vec4 metallic_roughness_occlusion_cutoff;
    uvec4 flags;
} material;
layout(set = 1, binding = 0) uniform texture2D base_image;
layout(set = 1, binding = 4) uniform sampler material_sampler;
void main() {
    // BLEND casts no shadow. MASK preserves its authored threshold. Texture
    // derivatives are computed before any divergent material/face discard.
    vec2 dx = dFdx(texcoord);
    vec2 dy = dFdy(texcoord);
    if (material.flags.x == 2u || (!gl_FrontFacing && material.flags.y == 0u)) { discard; }
    float alpha = clamp(material.base_factor.a * textureGrad(
        sampler2D(base_image, material_sampler), texcoord, dx, dy).a, 0.0, 1.0);
    if (material.flags.x == 1u && alpha < material.metallic_roughness_occlusion_cutoff.w) { discard; }
}
