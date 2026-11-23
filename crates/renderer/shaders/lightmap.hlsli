// Page texels contain linear diffuse irradiance for consecutive lighting keys.
// Rectangles include a filtering gutter. The descriptor selects the page pair.
float3 surface_texel(uint index, uint2 dims, uint keys, float phase, uint start) {
    uint at = start + index * keys + (uint)phase;
    float3 value = probe_load(at, dims).rgb;
    if (frac(phase) > 0.0)
        value = lerp(value, probe_load(at + 1u, dims).rgb, frac(phase));
    return value;
}
void apply_lightmap(inout Surface s, uint slot, bool front, float2 uv) {
    if (baked_debug() > 0u) { s.occlusion = 1.0; s.emissive = 0.0; }
    if (slot == 0u || camera.shadow_bias.w < 1.0) return;
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
    float phase = saturate(baked_blend()) * (keys - 1u);
    float3 a = lerp(surface_texel(i,dims,keys,phase,start),surface_texel(i+1u,dims,keys,phase,start),frac(coord.x));
    float3 b = lerp(surface_texel(i+pitch,dims,keys,phase,start),surface_texel(i+pitch+1u,dims,keys,phase,start),frac(coord.x));
    float3 irradiance = lerp(a,b,frac(coord.y));
    if (baked_debug() == 0u)
        s.emissive += irradiance * s.base_color * (1.0 - s.metallic) * s.occlusion / PI;
    else
        s.emissive = baked_debug() == 1u ? irradiance / PI : float3(0.9, 0.3, 0.04);
    s.occlusion = 0.0;
}
