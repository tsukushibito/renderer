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
// Each bit identifies one of the 16 prepared world-space capsules.
float indirect_visibility(float3 position, float3 normal, float2 pixel) {
    uint2 tile = min(uint2(pixel * camera.shadow_tile_scale.xy), 15u);
    uint index = tile.y * 16u + tile.x;
    uint mask = camera.shadow_masks[index / 4u][index % 4u] & 65535u;
    float occlusion = 0;
    [loop] while (mask != 0u) {
        uint i = uint(firstbitlow(mask)) * 3u;
        mask &= mask - 1u;
        float4 a = camera.capsule_data[i];
        float3 segment = camera.capsule_data[i+1u].xyz - a.xyz;
        float4 lighting = camera.capsule_data[i+2u];
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
#endif
void apply_lightmap(inout Surface s, uint slot, bool front, float2 uv
#ifdef INDIRECT_SHADOW
    , float3 position, float2 pixel
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
    irradiance *= indirect_visibility(position, s.geometric_normal, pixel);
#endif
    if (baked_debug() == 0u)
        s.emissive += irradiance * s.base_color * (1.0 - s.metallic) * s.occlusion / PI;
    else
        s.emissive = baked_debug() == 1u ? irradiance / PI : float3(0.9, 0.3, 0.04);
    s.occlusion = 0.0;
}
