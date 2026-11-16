#version 450 core

layout(location = 0) in vec3 world_position;
layout(location = 1) in vec3 world_normal;
layout(location = 2) in vec4 world_tangent;
layout(location = 3) in vec2 texcoord;
layout(location = 0) out vec4 output_color;
layout(set = 0, binding = 0, std140) uniform Camera {
    mat4 view_projection;
    vec4 eye;
    vec4 light_direction;
    vec4 light_color;
    vec4 ambient;
} camera;
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

const float PI = 3.14159265358979323846;

vec3 unit_or(vec3 value, vec3 fallback) {
    float squared = dot(value, value);
    return squared > 1e-20 ? value * inversesqrt(squared) : fallback;
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
    vec3 t = unit_or(world_tangent.xyz - n * dot(n, world_tangent.xyz),
        abs(n.z) < 0.9 ? unit_or(cross(vec3(0.0, 0.0, 1.0), n), vec3(1.0, 0.0, 0.0))
                      : unit_or(cross(vec3(0.0, 1.0, 0.0), n), vec3(1.0, 0.0, 0.0)));
    vec3 b = cross(n, t) * (world_tangent.w < 0.0 ? -1.0 : 1.0);
    vec3 mapped = textureGrad(sampler2D(normal_image, material_sampler), texcoord, dx, dy).xyz * 2.0 - 1.0;
    mapped.xy *= material.emissive_normal_scale.w;
    n = unit_or(t * mapped.x + b * mapped.y + n * mapped.z, n);
    if (!gl_FrontFacing) { n = -n; }
    vec3 v = unit_or(camera.eye.xyz - world_position, n);
    vec3 l = unit_or(camera.light_direction.xyz, n);
    vec3 h = unit_or(v + l, n);
    float nl = max(dot(n, l), 0.0);
    float nv = max(dot(n, v), 0.0);
    float nh = max(dot(n, h), 0.0);
    float vh = max(dot(v, h), 0.0);

    // Isotropic GGX with correlated Smith visibility and Schlick Fresnel.
    // The positive alpha floor regularizes the ideal roughness-zero singularity;
    // authored material values remain unchanged in the cooked input/manifest.
    float a = max(roughness * roughness, 1e-3);
    float a2 = a * a;
    float denominator = nh * nh * (a2 - 1.0) + 1.0;
    float distribution = a2 / max(PI * denominator * denominator, 1e-12);
    float visibility_l = nv * sqrt(a2 + (1.0 - a2) * nl * nl);
    float visibility_v = nl * sqrt(a2 + (1.0 - a2) * nv * nv);
    float visibility = 0.5 / max(visibility_l + visibility_v, 1e-6);
    vec3 f0 = mix(vec3(0.04), base.rgb, metallic);
    float schlick = pow(clamp(1.0 - vh, 0.0, 1.0), 5.0);
    vec3 fresnel = f0 + (vec3(1.0) - f0) * schlick;
    vec3 diffuse_color = base.rgb * (1.0 - metallic);
    vec3 diffuse = (vec3(1.0) - fresnel) * diffuse_color / PI;
    vec3 specular = fresnel * distribution * visibility;
    vec3 emissive = material.emissive_normal_scale.rgb * textureGrad(
        sampler2D(emissive_image, material_sampler), texcoord, dx, dy).rgb;
    // AO only modulates the explicit diffuse ambient fill, never direct light.
    vec3 color = (diffuse + specular) * camera.light_color.rgb * nl
        + camera.ambient.rgb * diffuse_color * ao + emissive;
    // The ideal low-roughness peak may exceed binary16's finite range. Preserve
    // finite linear target values; final SDR clamping remains a separate pass.
    output_color = vec4(clamp(color, vec3(0.0), vec3(65504.0)), alpha_mode == 2u ? alpha : 1.0);
}
