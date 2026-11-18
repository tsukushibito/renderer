#version 450 core

layout(location = 1) in vec3 world_normal;
layout(location = 2) in vec4 world_tangent;
layout(location = 3) in vec2 texcoord;
layout(location = 0) out vec4 base_metallic;
layout(location = 1) out vec4 normal_roughness;
layout(location = 2) out vec4 emissive_occlusion;
layout(set = 0, binding = 1, std140) uniform Material {
    vec4 base_factor;
    vec4 emissive_normal_scale;
    vec4 metallic_roughness_occlusion_cutoff;
    uvec4 flags;
} material;
layout(set = 1, binding = 0) uniform texture2D base_image;
layout(set = 1, binding = 1) uniform texture2D normal_image;
layout(set = 1, binding = 2) uniform texture2D orm_image;
layout(set = 1, binding = 3) uniform texture2D emissive_image;
layout(set = 1, binding = 4) uniform sampler material_sampler;

vec3 unit_or(vec3 value, vec3 fallback) {
    float squared = dot(value, value);
    return squared > 1e-20 ? value * inversesqrt(squared) : fallback;
}

vec2 oct_wrap(vec2 components) {
    vec2 sign_nonzero = mix(vec2(-1.0), vec2(1.0), greaterThanEqual(components, vec2(0.0)));
    return (1.0 - abs(components.yx)) * sign_nonzero;
}

vec2 oct_encode(vec3 direction) {
    float sum = abs(direction.x) + abs(direction.y) + abs(direction.z);
    vec2 oct = direction.xy / max(sum, 1e-20);
    return direction.z >= 0.0 ? oct : oct_wrap(oct);
}

// Two 7-bit octahedral coordinates occupy the payload of a finite normal
// binary16 value. Decode to an exact FP32 value before writing RGBA16F.
float pack_geometric_normal(vec3 direction) {
    vec2 encoded = oct_encode(direction) * 0.5 + 0.5;
    if (any(isnan(encoded))) { encoded = vec2(0.5); }
    uvec2 quantized = uvec2(roundEven(clamp(encoded, 0.0, 1.0) * 127.0));
    return unpackHalf2x16(0x0400u + quantized.x + (quantized.y << 7u)).x;
}

void main() {
    // Form derivatives while every fragment/helper invocation is active. Later
    // culling/alpha discards cannot make the four textureGrad LODs divergent.
    vec2 dx = dFdx(texcoord);
    vec2 dy = dFdy(texcoord);
    if (!gl_FrontFacing && material.flags.y == 0u) { discard; }
    vec4 base = material.base_factor * textureGrad(
        sampler2D(base_image, material_sampler), texcoord, dx, dy);
    float alpha = clamp(base.a, 0.0, 1.0);
    uint alpha_mode = material.flags.x;
    if ((alpha_mode == 1u && alpha < material.metallic_roughness_occlusion_cutoff.w)
        || (alpha_mode == 2u && alpha <= 0.0)) { discard; }

    vec3 orm = textureGrad(sampler2D(orm_image, material_sampler), texcoord, dx, dy).rgb;
    float metallic = clamp(material.metallic_roughness_occlusion_cutoff.x * orm.b, 0.0, 1.0);
    float roughness = clamp(material.metallic_roughness_occlusion_cutoff.y * orm.g, 0.0, 1.0);
    float ao = mix(1.0, orm.r, clamp(material.metallic_roughness_occlusion_cutoff.z, 0.0, 1.0));
    vec3 n = unit_or(world_normal, vec3(0.0, 0.0, 1.0));
    vec3 geometric_normal = gl_FrontFacing ? n : -n;
    vec3 t = unit_or(world_tangent.xyz - n * dot(n, world_tangent.xyz),
        abs(n.z) < 0.9 ? unit_or(cross(vec3(0.0, 0.0, 1.0), n), vec3(1.0, 0.0, 0.0))
                      : unit_or(cross(vec3(0.0, 1.0, 0.0), n), vec3(1.0, 0.0, 0.0)));
    vec3 b = cross(n, t) * (world_tangent.w < 0.0 ? -1.0 : 1.0);
    vec3 mapped = textureGrad(sampler2D(normal_image, material_sampler), texcoord, dx, dy).xyz * 2.0 - 1.0;
    mapped.xy *= material.emissive_normal_scale.w;
    n = unit_or(t * mapped.x + b * mapped.y + n * mapped.z, n);
    if (!gl_FrontFacing) { n = -n; }
    vec3 emissive = material.emissive_normal_scale.rgb * textureGrad(
        sampler2D(emissive_image, material_sampler), texcoord, dx, dy).rgb;
    base_metallic = vec4(base.rgb, metallic);
    normal_roughness = vec4(oct_encode(n), roughness, pack_geometric_normal(geometric_normal));
    emissive_occlusion = vec4(min(emissive, vec3(65504.0)), ao);
}
