#include "pbr_math.hlsli"
#include "raster_abi.hlsli"
#include "surface.hlsli"

ConstantBuffer<CameraData> camera : register(b0, space0);
Texture2D<float4> irradiance_image : register(t0, space2);
Texture2D<float4> reflection_image : register(t1, space2);
Texture2D<float4> brdf_image : register(t2, space2);
SamplerState environment_sampler : register(s3, space2);
Texture2D<float4> shadow_image0 : register(t0, space3);
Texture2D<float4> shadow_image1 : register(t1, space3);
Texture2D<float4> shadow_image2 : register(t2, space3);
Texture2D<float4> shadow_image3 : register(t3, space3);
SamplerState shadow_sampler : register(s4, space3);

#include "probe_volume.hlsli"

// Processed environment is top-origin lat-long. Rotate a world direction by
// inverse source yaw; U wraps while V clamps, including the pole texels.
float2 environment_uv(float3 direction) {
    float c = cos(camera.environment.y);
    float s = sin(camera.environment.y);
    float3 d = float3(c * direction.x - s * direction.z, direction.y,
                      s * direction.x + c * direction.z);
    // Longitude is arbitrary at a pole. Avoid atan2(0, 0).
    float u = dot(d.xz, d.xz) > 1e-20 ? frac(0.5 + atan2(d.z, d.x) / (2.0 * PI)) : 0.5;
    return float2(u, acos(clamp(d.y, -1.0, 1.0)) / PI);
}

float shadow_depth(uint cascade, float2 uv) {
    if (cascade == 0u) { return shadow_image0.SampleLevel(shadow_sampler, uv, 0.0).r; }
    if (cascade == 1u) { return shadow_image1.SampleLevel(shadow_sampler, uv, 0.0).r; }
    if (cascade == 2u) { return shadow_image2.SampleLevel(shadow_sampler, uv, 0.0).r; }
    return shadow_image3.SampleLevel(shadow_sampler, uv, 0.0).r;
}

float cascade_visibility(uint cascade, float3 position, float3 geometric_normal, float3 light) {
    cascade = min(cascade, 3u);
    float slope = 1.0 - max(dot(geometric_normal, light), 0.0);
    float texel_world = camera.shadow_texel_world[cascade];
    float3 receiver = position + geometric_normal *
        (texel_world * camera.shadow_bias.z * slope);
    float4 clip = mul(float4(receiver, 1.0), camera.shadow_view_projection[cascade]);
    // Orthographic matrices have w=1. Guard the trusted shader's arithmetic for
    // any finite payload accepted by the generic UBO API as well.
    if (!(clip.w > 1e-20)) { return 1.0; }
    float3 ndc = clip.xyz / clip.w;
    // The camera matrices use top-left logical clip coordinates for both APIs.
    float2 uv = ndc.xy * 0.5 + 0.5;
    if (any(uv < 0.0) || any(uv > 1.0) || ndc.z < 0.0 || ndc.z > 1.0) { return 1.0; }
    float depth_bias = texel_world * camera.shadow_depth_scale[cascade] *
        (camera.shadow_bias.x + camera.shadow_bias.y * slope);
    float reference = ndc.z - depth_bias;
    float2 texel = 1.0 / texture_size(shadow_image0);
    float visible = 0.0;
    for (int y = -1; y <= 1; ++y) {
        for (int x = -1; x <= 1; ++x) {
            float2 tap = uv + float2(x, y) * texel;
            // Samples outside the shadow frustum are lit, rather than an
            // implicit black border or an extended edge caster.
            if (any(tap < 0.0) || any(tap > 1.0)) {
                visible += 1.0;
            } else {
                visible += reference <= shadow_depth(cascade, tap) ? 1.0 : 0.0;
            }
        }
    }
    return visible / 9.0;
}

float sun_visibility(float3 position, float3 geometric_normal, float3 light) {
    if (!(camera.environment.w > 0.0)) { return 1.0; }
    float distance = dot(position - camera.eye.xyz, camera.camera_forward_blend.xyz);
    if (distance < camera.eye.w || distance > camera.shadow_splits.w) { return 1.0; }
    uint cascade = distance <= camera.shadow_splits.x ? 0u :
        (distance <= camera.shadow_splits.y ? 1u : (distance <= camera.shadow_splits.z ? 2u : 3u));
    float visible = cascade_visibility(cascade, position, geometric_normal, light);
    float start = cascade == 0u ? camera.eye.w : camera.shadow_splits[max(cascade, 1u) - 1u];
    float end = camera.shadow_splits[cascade];
    float width = max((end - start) * clamp(camera.camera_forward_blend.w, 0.0, 1.0), 1e-6);
    float weight = clamp((distance - (end - width)) / width, 0.0, 1.0);
    if (weight > 0.0) {
        float next = cascade < 3u ? cascade_visibility(cascade + 1u, position, geometric_normal, light) : 1.0;
        visible = lerp(visible, next, weight);
    }
    return visible;
}

struct BrdfInputs {
    float3 view;
    float3 f0;
    float3 diffuse_color;
};

BrdfInputs make_brdf_inputs(float3 position, Surface surface) {
    BrdfInputs result;
    float3 to_eye = camera.light_color.w > 0.0
        ? -camera.camera_forward_blend.xyz : camera.eye.xyz - position;
    result.view = unit_or(to_eye, surface.normal);
    result.f0 = lerp(0.04, surface.base_color, surface.metallic);
    result.diffuse_color = surface.base_color * (1.0 - surface.metallic);
    return result;
}

float3 evaluate_lighting(float3 position, Surface surface, BrdfInputs brdf_inputs) {
    float3 n = surface.normal;
    if (baked_debug()>0u) {
        if(surface.occlusion==0.0)return surface.emissive;
        float3 value=probe_irradiance(position,n,baked_blend());
        return baked_debug()==1u ? value/PI : value;
    }
    float3 v = brdf_inputs.view;
    float3 l = unit_or(camera.light_direction.xyz, n);
    float3 h = unit_or(v + l, n);
    float nl = max(dot(n, l), 0.0);
    float nv = max(dot(n, v), 0.0);
    float nh = max(dot(n, h), 0.0);
    float vh = max(dot(v, h), 0.0);
    float3 f0 = brdf_inputs.f0;
    float3 diffuse_color = brdf_inputs.diffuse_color;
    float roughness = surface.roughness;
    float3 direct_brdf = evaluate_brdf(nl, nv, nh, vh, f0, diffuse_color, roughness);
    float environment_strength = max(camera.environment.x, 0.0);
    float3 indirect = 0.0;
    [branch]
    if (environment_strength > 0.0 || camera.shadow_bias.w >= 1.0) {
        float3 irradiance;
        [branch]
        if (camera.shadow_bias.w >= 1.0) {
            irradiance = surface.occlusion > 0.0 ? probe_irradiance(position, n, baked_blend()) : 0.0;
        } else {
            irradiance = irradiance_image.SampleLevel(environment_sampler, environment_uv(n), 0.0).rgb;
        }
        float3 reflected = reflect(-v, n);
        float reflection_lod = roughness * max(camera.environment.z, 0.0);
        float3 prefiltered = reflection_image.SampleLevel(
            environment_sampler, environment_uv(reflected), reflection_lod).rgb;
        // BRDF and environment share one sampler. Explicit LOD0 and
        // half-texel interior UVs implement LUT clamp.
        float2 lut_size = texture_size(brdf_image);
        float2 lut_uv = clamp(float2(nv, roughness), 0.5 / lut_size, 1.0 - 0.5 / lut_size);
        float2 brdf = brdf_image.SampleLevel(environment_sampler, lut_uv, 0.0).rg;
        // AO attenuates only diffuse environment light.
        float3 indirect_diffuse = irradiance * diffuse_color / PI * surface.occlusion;
        float3 indirect_specular = prefiltered * (f0 * brdf.x + brdf.y);
        indirect = camera.shadow_bias.w >= 1.0
            ? indirect_diffuse + indirect_specular * environment_strength
            : (indirect_diffuse + indirect_specular) * environment_strength;
    }
    // At exactly zero N dot L, finite shadow visibility can only multiply
    // zero. Shadow sampling uses explicit LOD, so this branch needs no gradients.
    float shadow_visibility = 1.0;
    if (nl != 0.0) { shadow_visibility = sun_visibility(position, surface.geometric_normal, l); }
    return direct_brdf * camera.light_color.rgb * nl * shadow_visibility + indirect + surface.emissive;
}
