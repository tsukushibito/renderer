// Scene RGB is premultiplied over transparent black. The remaining coverage
// transmits the directional sky, including through blended geometry.
Texture2D<float4> scene : register(t0, space0);
Texture2D<float4> sky : register(t1, space0);
SamplerState sky_linear : register(s2, space0);
struct FilmParameters {
    float4 corner_scale;
    float4 step_x;
    float4 step_y;
    float4 sensor_r;
    float4 sensor_g;
    float4 sensor_b;
};
#ifdef __spirv__
[[vk::push_constant]]
#endif
ConstantBuffer<FilmParameters> film : register(b7, space0);

float2 equal_area(float3 dir) {
    float r = sqrt(max(0.0, 1.0 - abs(dir.z)));
    float a = (dir.x == 0.0 && dir.y == 0.0) ? 0.0 : atan2(abs(dir.y), abs(dir.x)) * 0.6366197724;
    float2 uv = r * float2(1.0-a, a);
    if (dir.z < 0.0) uv = 1.0 - uv.yx;
    return 0.5 + 0.5 * uv * float2(dir.x < 0.0 ? -1.0 : 1.0, dir.y < 0.0 ? -1.0 : 1.0);
}

// Across an equal-area square edge the adjacent hemisphere reverses the
// coordinate along that edge. Both reflections apply at a corner.
float3 sky_tap(int2 p, int2 size) {
    if (p.x < 0 || p.x >= size.x) {
        p.x = clamp(p.x, 0, size.x - 1);
        p.y = size.y - 1 - p.y;
    }
    if (p.y < 0 || p.y >= size.y) {
        p.y = clamp(p.y, 0, size.y - 1);
        p.x = size.x - 1 - p.x;
    }
    return sky.Load(int3(p, 0)).rgb;
}

float3 sky_radiance(float2 uv, int2 size) {
    if (film.step_x.w == 0) {
        return sky.Load(int3(min(int2(uv * size), size - 1), 0)).rgb;
    }
    float2 p = uv * size - 0.5;
    if (all(p >= 0) && all(p < size - 1)) {
        return sky.SampleLevel(sky_linear, uv, 0).rgb;
    }
    int2 base = int2(floor(p));
    float2 t = frac(p);
    return lerp(lerp(sky_tap(base, size), sky_tap(base + int2(1,0), size), t.x),
                lerp(sky_tap(base + int2(0,1), size), sky_tap(base + 1, size), t.x), t.y);
}

float4 main(float4 position : SV_Position) : SV_Target0 {
    float4 surface = scene.Load(int3(int2(position.xy), 0));
    float3 radiance = surface.rgb;
    if (surface.a < 1.0) {
        uint w,h;
        sky.GetDimensions(w,h);
        float3 ray = normalize(film.corner_scale.xyz + position.x*film.step_x.xyz + position.y*film.step_y.xyz);
        radiance += (1.0-surface.a)*film.corner_scale.w*sky_radiance(equal_area(ray), int2(w,h));
    }
    return float4(dot(film.sensor_r.xyz,radiance), dot(film.sensor_g.xyz,radiance), dot(film.sensor_b.xyz,radiance), 1.0);
}
