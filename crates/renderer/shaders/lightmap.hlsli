// Triangle-local UV1 charts. Vertices of static receivers are expanded in
// triangle order at cook time. UV0 and the material texture chains stay intact.
float3 chart_sample(uint start, uint pixel, uint2 dims, float mix, uint scenarios) {
    uint i = start + scenarios * pixel;
    float3 value = probe_load(i,dims).rgb;
    [branch] if (mix > 0.0) value = lerp(value, probe_load(i+1u,dims).rgb, mix);
    return value;
}
void apply_lightmap(inout Surface s, uint first, uint first_vertex, uint vertex, float2 uv) {
    if (baked_debug() > 0u) {
        s.occlusion = 1.0;
        s.emissive = 0.0;
    }
    if (first == 0u || camera.shadow_bias.w < 1.0)
        return;
    if (vertex < first_vertex)
        return;
    uint triangle_id = (vertex - first_vertex) / 3u;
    float3 bary = float3(1.0 - uv.x - uv.y, uv);
    uint phase = first_vertex % 3u;
    uv = float2(bary[(phase + 1u) % 3u], bary[(phase + 2u) % 3u]);
    uint2 dims;
    irradiance_image.GetDimensions(dims.x, dims.y);
    uint last = dims.x * dims.y - 1u;
    float4 header = probe_load(1u, dims);
    uint scenarios = lighting_keys(header.w);
    uint stride = 2u+4u*scenarios;
    uint charts = probe_index(header.z, last);
    if (first > charts || triangle_id > charts - first)
        return;
    uint start =
        2u + max(scenarios,2u) + 3u * probe_index(header.x, last / 3u) + stride * probe_index(header.y, last / stride);
    float4 chart = probe_load(start + first - 1u + triangle_id, dims);
    uint width = max(probe_index(chart.y, 256u), 2u), height = max(probe_index(chart.z, 256u), 2u);
    float2 xy = saturate(uv) * float2(width - 1u, height - 1u);
    uint2 lo = (uint2)xy, hi = min(lo + 1u, uint2(width - 1u, height - 1u));
    uint at = probe_index(chart.x, last);
    float key_position = saturate(baked_blend()) * (scenarios-1u);
    at += (uint)key_position;
    float t = frac(key_position);
    float3 a = lerp(chart_sample(at, lo.y * width + lo.x, dims, t, scenarios),
                    chart_sample(at, lo.y * width + hi.x, dims, t, scenarios), frac(xy.x));
    float3 b = lerp(chart_sample(at, hi.y * width + lo.x, dims, t, scenarios),
                    chart_sample(at, hi.y * width + hi.x, dims, t, scenarios), frac(xy.x));
    float3 irradiance = max(lerp(a, b, frac(xy.y)), 0.0);
    if (baked_debug() == 0u)
        s.emissive += irradiance * s.base_color * (1.0 - s.metallic) * s.occlusion / PI;
    else
        s.emissive = baked_debug() == 1u ? irradiance / PI : float3(0.9, 0.3, 0.04);
    // This receiver's diffuse response has been resolved in its chart.
    s.occlusion = 0.0;
}
