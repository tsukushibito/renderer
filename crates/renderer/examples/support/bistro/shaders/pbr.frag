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
    vec4 environment; // strength, inverse-yaw input radians, specular max LOD, shadows enabled
    mat4 shadow_view_projection[4];
    vec4 shadow_splits;
    vec4 shadow_texel_world;
    vec4 shadow_depth_scale;
    vec4 camera_forward_blend;
    vec4 shadow_bias; // receiver constant/slope/normal offset in world-texel units
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

layout(set = 2, binding = 0) uniform texture2D irradiance_image;
layout(set = 2, binding = 1) uniform texture2D reflection_image;
layout(set = 2, binding = 2) uniform texture2D brdf_image;
layout(set = 2, binding = 3) uniform sampler environment_sampler;
layout(set = 3, binding = 0) uniform texture2D shadow_image0;
layout(set = 3, binding = 1) uniform texture2D shadow_image1;
layout(set = 3, binding = 2) uniform texture2D shadow_image2;
layout(set = 3, binding = 3) uniform texture2D shadow_image3;
layout(set = 3, binding = 4) uniform sampler shadow_sampler;

const float PI = 3.14159265358979323846;

vec3 unit_or(vec3 value, vec3 fallback) {
    float squared = dot(value, value);
    return squared > 1e-20 ? value * inversesqrt(squared) : fallback;
}

// Processed environment is top-origin lat-long. Rotate a world direction by
// inverse source yaw; U wraps while V clamps, including the pole texels.
vec2 environment_uv(vec3 direction) {
    float c = cos(camera.environment.y);
    float s = sin(camera.environment.y);
    vec3 d = vec3(c * direction.x - s * direction.z, direction.y,
                  s * direction.x + c * direction.z);
    // Longitude is arbitrary at a pole. Avoid GLSL's undefined atan(0,0).
    float u = dot(d.xz, d.xz) > 1e-20 ? fract(0.5 + atan(d.z, d.x) / (2.0 * PI)) : 0.5;
    return vec2(u, acos(clamp(d.y, -1.0, 1.0)) / PI);
}

float shadow_depth(uint cascade, vec2 uv) {
    if (cascade == 0u) { return textureLod(sampler2D(shadow_image0, shadow_sampler), uv, 0.0).r; }
    if (cascade == 1u) { return textureLod(sampler2D(shadow_image1, shadow_sampler), uv, 0.0).r; }
    if (cascade == 2u) { return textureLod(sampler2D(shadow_image2, shadow_sampler), uv, 0.0).r; }
    return textureLod(sampler2D(shadow_image3, shadow_sampler), uv, 0.0).r;
}

float cascade_visibility(uint cascade, vec3 geometric_normal, vec3 light) {
    cascade = min(cascade, 3u);
    float slope = 1.0 - max(dot(geometric_normal, light), 0.0);
    float texel_world = camera.shadow_texel_world[cascade];
    vec3 receiver = world_position + geometric_normal *
        (texel_world * camera.shadow_bias.z * slope);
    vec4 clip = camera.shadow_view_projection[cascade] * vec4(receiver, 1.0);
    // Orthographic matrices have w=1. Guard the trusted shader's arithmetic for
    // any finite payload accepted by the generic UBO API as well.
    if (!(clip.w > 1e-20)) { return 1.0; }
    vec3 ndc = clip.xyz / clip.w;
    vec2 uv = ndc.xy * 0.5 + 0.5;
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))
        || ndc.z < 0.0 || ndc.z > 1.0) { return 1.0; }
    float depth_bias = texel_world * camera.shadow_depth_scale[cascade] *
        (camera.shadow_bias.x + camera.shadow_bias.y * slope);
    float reference = ndc.z - depth_bias;
    vec2 texel = 1.0 / vec2(textureSize(sampler2D(shadow_image0, shadow_sampler), 0));
    float visible = 0.0;
    for (int y = -1; y <= 1; ++y) {
        for (int x = -1; x <= 1; ++x) {
            vec2 tap = uv + vec2(x, y) * texel;
            // Samples outside the shadow frustum are lit, rather than an
            // implicit black border or an extended edge caster.
            if (any(lessThan(tap, vec2(0.0))) || any(greaterThan(tap, vec2(1.0)))) {
                visible += 1.0;
            } else {
                visible += reference <= shadow_depth(cascade, tap) ? 1.0 : 0.0;
            }
        }
    }
    return visible / 9.0;
}

float sun_visibility(vec3 geometric_normal, vec3 light) {
    if (!(camera.environment.w > 0.0)) { return 1.0; }
    float distance = dot(world_position - camera.eye.xyz, camera.camera_forward_blend.xyz);
    if (distance < camera.eye.w || distance > camera.shadow_splits.w) { return 1.0; }
    uint cascade = distance <= camera.shadow_splits.x ? 0u :
        (distance <= camera.shadow_splits.y ? 1u : (distance <= camera.shadow_splits.z ? 2u : 3u));
    float visible = cascade_visibility(cascade, geometric_normal, light);
    float start = cascade == 0u ? camera.eye.w : camera.shadow_splits[max(cascade, 1u) - 1u];
    float end = camera.shadow_splits[cascade];
    float width = max((end - start) * clamp(camera.camera_forward_blend.w, 0.0, 1.0), 1e-6);
    float weight = clamp((distance - (end - width)) / width, 0.0, 1.0);
    if (weight > 0.0) {
        float next = cascade < 3u ? cascade_visibility(cascade + 1u, geometric_normal, light) : 1.0;
        visible = mix(visible, next, weight);
    }
    return visible;
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
    vec3 irradiance = textureLod(sampler2D(irradiance_image, environment_sampler), environment_uv(n), 0.0).rgb;
    vec3 reflected = reflect(-v, n);
    float reflection_lod = roughness * max(camera.environment.z, 0.0);
    vec3 prefiltered = textureLod(sampler2D(reflection_image, environment_sampler),
        environment_uv(reflected), reflection_lod).rgb;
    // BRDF and environment share one sampler to stay within the common 16-slot
    // contract. Explicit LOD0 and half-texel interior UVs implement LUT clamp.
    vec2 lut_size = vec2(textureSize(sampler2D(brdf_image, environment_sampler), 0));
    vec2 lut_uv = clamp(vec2(nv, roughness), 0.5 / lut_size, vec2(1.0) - 0.5 / lut_size);
    vec2 brdf = textureLod(sampler2D(brdf_image, environment_sampler), lut_uv, 0.0).rg;
    vec3 indirect_diffuse = irradiance * diffuse_color / PI * ao;
    vec3 indirect_specular = prefiltered * (f0 * brdf.x + brdf.y);
    // AO is an authored diffuse-environment term, not a direct-light shadow or
    // an attenuation of emissive/specular contributions.
    vec3 color = (diffuse + specular) * camera.light_color.rgb * nl *
        sun_visibility(geometric_normal, l)
        + (indirect_diffuse + indirect_specular) * max(camera.environment.x, 0.0) + emissive;
    // The ideal low-roughness peak may exceed binary16's finite range. Preserve
    // finite linear target values; final SDR clamping remains a separate pass.
    output_color = vec4(clamp(color, vec3(0.0), vec3(65504.0)), alpha_mode == 2u ? alpha : 1.0);
}
