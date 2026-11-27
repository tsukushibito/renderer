// A workgroup integrates one film pixel. Each lane traces a distinct lens
// and subpixel sample. The host combines successive groups of up to 64 samples.
float filterOffset(float u) {
    const uint table = 96 + 64*16;
    uint lo = 0, hi = 2048;
    while (lo+1 < hi) {
        uint mid = (lo+hi)/2;
        if (asfloat(film.Load(table + 4*mid)) <= u) lo = mid;
        else hi = mid;
    }
    float2 edge = asfloat(film.Load2(table + 4*lo));
    return (lo + (u-edge.x)/max(edge.y-edge.x, 1e-30)) * (3.0/2048) - 1.5;
}

groupshared float3 sampleSum[64];

[numthreads(64,1,1)]
void main(uint3 group : SV_GroupID, uint lane : SV_GroupIndex) {
    uint4 rect = film.Load4(0); // crop origin, width, linear offset
    uint pixel = rect.w + group.x;
    uint2 xy = rect.xy + uint2(pixel % rect.z, pixel / rect.z);
    uint2 samples = film.Load2(16); // first index and number of active lanes
    float3 value = 0;
    if (lane < samples.y) {
        uint key = hash32(xy.x ^ hash32(xy.y));
        float4 shift = uint4(hash32(key^2),hash32(key^3),hash32(key^5),hash32(key^7)) >> 8;
        float4 u = frac(asfloat(film.Load4(96 + lane*16)) + shift * (1.0/16777216.0));
        float2 centre = asfloat(film.Load2(24));
        float4 eye = asfloat(film.Load4(32)); // eye.xyz, aperture radius
        float4 forward = asfloat(film.Load4(48)); // forward.xyz, focus distance
        float4 right = asfloat(film.Load4(64)); // right.xyz, pixel scale
        float3 up = asfloat(film.Load3(80));
        float2 plane = (float2(xy) + .5 + float2(filterOffset(u.x), filterOffset(u.y)) - centre) * right.w;
        float sn, cs;
        sincos(2*pi*u.w, sn, cs);
        float3 offset = eye.w * sqrt(u.z) * (cs*right.xyz + sn*up);
        FilmRay camera;
        camera.origin = eye.xyz + offset;
        camera.direction = normalize(forward.xyz + plane.x*right.xyz - plane.y*up - offset/forward.w);
        camera.seed = hash32(key ^ hash32(samples.x + lane));
        camera.reserved = 0;
        value = integrate(camera);
    }
    sampleSum[lane] = value;
    GroupMemoryBarrierWithGroupSync();
    [unroll]
    for (uint stride = 32; stride > 0; stride >>= 1) {
        if (lane < stride) sampleSum[lane] += sampleSum[lane + stride];
        GroupMemoryBarrierWithGroupSync();
    }
    if (lane == 0) hits[group.x] = uint4(asuint(sampleSum[0]),0);
}
