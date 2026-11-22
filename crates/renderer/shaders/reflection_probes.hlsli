// Layout matches ReflectionProbes: header + two sets of 127 filtered map rows.
// Each level occupies the full width to preserve seamless longitude filtering.
Texture2D<float4> reflection_probes : register(t4, space2);

float3 probe_level(float2 uv, uint level, uint first_row, float atlas_height) {
    uint rows = 64u >> level;
    float v = float(first_row + 128u - 2u * rows)
        + clamp(uv.y * rows, 0.5, rows - 0.5);
    return reflection_probes.SampleLevel(environment_sampler, float2(uv.x, v / atlas_height), 0).rgb;
}

float3 probe_specular(uint row, float3 ray, float roughness, float blend, float height, bool single) {
    float2 uv = float2(dot(ray.xz, ray.xz) > 1e-20 ? 0.5 + atan2(ray.z, ray.x) / (2.0 * PI) : 0.5,
                       acos(clamp(ray.y, -1.0, 1.0)) / PI);
    float lod = saturate(roughness) * 6.0;
    uint a = uint(lod);
    uint b = min(a+1u,6u);
    if (!single && blend == 1.0) row += 127u;
    float3 day = lerp(probe_level(uv,a,row,height), probe_level(uv,b,row,height), frac(lod));
    [branch] if (single || blend == 0.0 || blend == 1.0) return day;
    float3 dusk = lerp(probe_level(uv,a,row+127u,height), probe_level(uv,b,row+127u,height), frac(lod));
    return lerp(day,dusk,blend);
}

float3 local_specular(float3 world, float3 reflected, float roughness, float3 global, float parameter,
                      out float3 weights) {
    uint width, height;
    reflection_probes.GetDimensions(width, height);
    float3 radiance = 0.0;
    float total = 0.0;
    weights = 0.0;
    float blend = saturate(parameter - floor(parameter * 0.5) * 2.0);
    // The bound also applies to arbitrary textures supplied to the safe graph API.
    bool single = height % 128u == 0u;
    uint rows = single ? 128u : 255u;
    uint count = width == 128u ? min(height / rows, 32u) : 0u;
    uint candidates = count == 32u ? 0xffffffffu : (1u << count) - 1u;
    if (count > 4u) {
        float4 origin = reflection_probes.Load(int3(3,0,0));
        if (origin.w == 1.0) {
            float3 scale = reflection_probes.Load(int3(4,0,0)).xyz;
            uint3 cell = (uint3)clamp((world-origin.xyz)*scale,0.0,3.0);
            float2 halves = reflection_probes.Load(int3(5u+cell.x+4u*cell.y+16u*cell.z,0,0)).xy;
            uint2 bits = (uint2)clamp(halves,0.0,65535.0);
            candidates &= bits.x | (bits.y << 16u);
        }
    }
    while (candidates != 0u) {
        uint i = firstbitlow(candidates);
        candidates &= candidates-1u;
        uint row = i*rows;
        float4 capture = reflection_probes.Load(int3(0,row,0));
        float3 lo = reflection_probes.Load(int3(1,row,0)).xyz;
        float3 hi = reflection_probes.Load(int3(2,row,0)).xyz;
        float3 inside = min(world-lo,hi-world);
        float distance = min(inside.x,min(inside.y,inside.z));
        if (!(distance > 0.0 && capture.w > 0.0)) continue;
        float w = saturate(distance / capture.w);
        // Intersect the outgoing ray with the nearest positive box face.
        float3 extent = lerp(world-lo, hi-world, step(0.0,reflected));
        float3 t = extent / max(abs(reflected),1e-20);
        float far_t = min(t.x,min(t.y,t.z));
        float3 ray = unit_or(world-capture.xyz+far_t*reflected,reflected);
        radiance += w * probe_specular(row+1u, ray, roughness, blend, float(height), single);
        total += w;
        weights[i%2u] += w;
    }
    float divisor = max(1.0,total);
    weights.xy /= divisor;
    weights.z = saturate(1.0-total);
    return radiance / divisor + weights.z*global;
}
