// Page texels contain linear diffuse irradiance for consecutive lighting keys.
// Rectangles include a filtering gutter. The descriptor selects the page pair.
float3 surface_texel(uint index, uint2 dims, uint keys, float phase, uint start) {
    uint at = start + index * keys + (uint)phase;
    float3 value = probe_load(at, dims).rgb;
    if (frac(phase) > 0.0)
        value = lerp(value, probe_load(at + 1u, dims).rgb, frac(phase));
    return value;
}
#ifdef INDIRECT_SHADOW
#if defined(SHADOW_HALF_RESOLVE) && BISTRO_LOCAL_LIGHTS == 2
#define shadow_table local_lights
#else
Texture2D<float4> shadow_table : register(t5, space0);
#endif
float4 shadow_texel(uint at, uint width) {
    return shadow_table.Load(int3(at % width, at / width, 0));
}
float indirect_visibility(float3 position, float3 normal, float2 pixel) {
    uint width, height;
    shadow_table.GetDimensions(width, height);
    uint texels = width * height;
    if (texels < 260u) return 1.0;
    uint2 tile = uint2(clamp(pixel * shadow_texel(0u, width).xy, 0.0, 15.0));
    float2 range = shadow_texel(1u + tile.y * 16u + tile.x, width).xy;
    uint first = min(uint(range.x), texels * 4u);
    uint count = min(uint(range.y), texels * 4u - first);
    float occlusion = 0;
    [loop] for (uint j = 0u; j < count; ++j) {
        uint word = first + j;
        uint i = min(uint(shadow_texel(word / 4u, width)[word % 4u]), texels - 3u);
        float4 a = shadow_texel(i, width);
        float3 segment = shadow_texel(i+1u, width).xyz - a.xyz;
        float4 lighting = shadow_texel(i+2u, width);
        float t = saturate(dot(position - a.xyz, segment) / max(dot(segment,segment), 1e-20));
        float3 delta = a.xyz + segment*t - position;
        float distance = length(delta);
        if (distance >= 6.0*a.w || dot(delta, normal) <= -a.w) continue;
        float3 axis = delta / max(distance, 1e-10);
        float ratio = saturate(a.w / max(distance, 1e-10));
        float angle = asin(ratio);
        float separation = acos(clamp(dot(axis,lighting.xyz), -1.0, 1.0));
        const float cone = 0.65;
        float overlap = smoothstep(0.0, 1.0, (angle + cone - separation) / (2.0*min(angle,cone)));
        float directional = overlap * saturate(angle*angle / (cone*cone));
        float contact = ratio*ratio*saturate(dot(axis,normal));
        float fade = 1.0 - smoothstep(4.0, 6.0, distance/a.w);
        occlusion = max(occlusion, fade * lerp(contact, directional, lighting.w));
    }
    return 1.0 - occlusion;
}
#if defined(SHADOW_HALF_RESOLVE) || defined(SHADOW_RECEIVER)
#include "octahedral.hlsli"
#endif
#ifdef SHADOW_RECEIVER
float4 half_receiver(float3 position, float3 normal, float3 pixel) {
    float2 full_extent = shadow_texel(0u, 1u).zw;
    float2 full_pixel = pixel.xy * full_extent / ceil(full_extent * 0.5);
    // Integers through 1024 are exact in RGBA16F. Splitting before the
    // attachment conversion also tolerates round-toward-zero stores.
    float scaled_depth = pixel.z * 1024.0;
    return float4(indirect_visibility(position, normal, full_pixel), floor(scaled_depth),
        frac(scaled_depth), pack_geometric_normal(normal));
}
#endif
#ifdef SHADOW_HALF_RESOLVE
Texture2D<float4> half_shadow : register(t6, space0);
float resolve_visibility(float3 position, float3 normal, float3 pixel, float2 slope) {
    uint w,h;
    half_shadow.GetDimensions(w,h);
    float2 scale = float2(w,h) / shadow_texel(0u, 1u).zw;
    float2 coord = pixel.xy * scale - 0.5;
    uint tw, th;
    shadow_table.GetDimensions(tw, th);
    uint2 tile = uint2(clamp(pixel.xy * shadow_texel(0u, tw).xy, 0.0, 15.0));
    if (1u + tile.y * 16u + tile.x >= tw * th ||
        shadow_texel(1u + tile.y * 16u + tile.x, tw).y == 0.0) return 1.0;
    float total = 0, visibility = 0, lowest = 1, highest = 0;
    [unroll] for (int y = 0; y < 2; ++y) {
        [unroll] for (int x = 0; x < 2; ++x) {
            int2 texel = clamp(int2(floor(coord)) + int2(x,y), int2(0,0), int2(w-1,h-1));
            float4 sample = half_shadow.Load(int3(texel,0));
            float z = (sample.y + sample.z) / 1024.0;
            float projected = pixel.z + dot((float2(texel)+0.5)/scale-pixel.xy, slope);
            if (sample.w == 0.0 || abs(z-projected)>1e-6 || dot(normal,unpack_geometric_normal(sample.w))<0.98) continue;
            float2 weights = lerp(1.0-frac(coord), frac(coord), float2(x,y));
            float weight = weights.x * weights.y;
            visibility += sample.x * weight;
            total += weight;
            lowest = min(lowest, sample.x);
            highest = max(highest, sample.x);
        }
    }
    if (total > 0.0 && highest - lowest < 0.1) return visibility / total;
    return indirect_visibility(position, normal, pixel.xy);
}
#endif
#endif
void apply_lightmap(inout Surface s, uint slot, bool front, float2 uv
#ifdef INDIRECT_SHADOW
    , float3 position, float3 pixel
#ifdef SHADOW_HALF_RESOLVE
    , float2 depth_slope
#endif
#endif
) {
    if (baked_debug() > 0u) { s.occlusion = 1.0; s.emissive = 0.0; }
    if (slot == 0u || camera.shadow_bias.w == 0.0) return;
    uint2 dims;
    irradiance_image.GetDimensions(dims.x, dims.y);
    uint last = dims.x * dims.y - 1u;
    float4 header = probe_load(0u, dims);
    if (slot > probe_index(header.x, last / 2u)) return;
    uint keys = clamp(probe_index(header.y, 16u), 1u, 16u);
    uint start = probe_index(header.z, last);
    float4 rect = probe_load(2u * slot - 1u, dims);
    uint origin = probe_index(front ? rect.x : rect.z, last);
    uint pitch = probe_index(front ? rect.y : rect.w, 2048u);
    float2 extent = clamp(probe_load(2u * slot, dims).xy, 1.0, 2048.0);
    float2 coord = saturate(uv) * extent - 0.5;
    int2 xy = (int2)floor(coord);
    uint i = uint(int(origin) + xy.x + xy.y * int(pitch));
    // A negative scenario word selects surface pages with ordinary environment diffuse.
    float blend = camera.shadow_bias.w < 0.0 ? saturate(-1.0 - camera.shadow_bias.w) : baked_blend();
    float phase = blend * (keys - 1u);
    float3 a = lerp(surface_texel(i,dims,keys,phase,start),surface_texel(i+1u,dims,keys,phase,start),frac(coord.x));
    float3 b = lerp(surface_texel(i+pitch,dims,keys,phase,start),surface_texel(i+pitch+1u,dims,keys,phase,start),frac(coord.x));
    float3 irradiance = lerp(a,b,frac(coord.y));
#ifdef INDIRECT_SHADOW
#ifdef SHADOW_HALF_RESOLVE
    irradiance *= resolve_visibility(position, s.geometric_normal, pixel, depth_slope);
#else
    irradiance *= indirect_visibility(position, s.geometric_normal, pixel.xy);
#endif
#endif
    if (baked_debug() == 0u)
        s.emissive += irradiance * s.base_color * (1.0 - s.metallic) * s.occlusion / PI;
    else
        s.emissive = baked_debug() == 1u ? irradiance / PI : float3(0.9, 0.3, 0.04);
    s.occlusion = 0.0;
}
