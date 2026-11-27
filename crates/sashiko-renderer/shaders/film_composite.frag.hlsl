// Scene RGB is premultiplied over transparent black. The remaining coverage
// transmits the directional sky, including through blended geometry.
Texture2D<float4> scene : register(t0, space0);
Texture2D<float4> sky : register(t1, space0);
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

float4 main(float4 position : SV_Position) : SV_Target0 {
    float4 surface = scene.Load(int3(int2(position.xy), 0));
    float3 radiance = surface.rgb;
    if (surface.a < 1.0) {
        uint w,h;
        sky.GetDimensions(w,h);
        float3 ray = normalize(film.corner_scale.xyz + position.x*film.step_x.xyz + position.y*film.step_y.xyz);
        uint2 texel = min(uint2(w-1,h-1), uint2(equal_area(ray)*float2(w,h)));
        radiance += (1.0-surface.a)*film.corner_scale.w*sky.Load(int3(texel,0)).rgb;
    }
    return float4(dot(film.sensor_r.xyz,radiance), dot(film.sensor_g.xyz,radiance), dot(film.sensor_b.xyz,radiance), 1.0);
}
