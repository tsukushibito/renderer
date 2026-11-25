// Real-Time Polygonal-Light Shading with Linearly Transformed Cosines,
// Heitz, Dupuy, Hill and Neubelt (SIGGRAPH 2016). Fit license: licenses/LTC.txt.
#include "ltc_fit.hlsli"

void ltc_coefficients(uint index, out float4 matrix, out float2 amplitude) {
    uint3 packed = LTC_FIT[index];
    float3 lo = f16tof32(packed & 65535u);
    float3 hi = f16tof32(packed >> 16);
    matrix = float4(lo.x, hi.x, lo.y, hi.y);
    amplitude = float2(lo.z, hi.z);
}

float rect_cosine_integral(float3 corners[4], float4 m) {
    // Clip the transformed polygon to the positive cosine hemisphere.
    // Four vertices can append at most six entries even for a malformed raw light.
    float3 clipped[6];
    uint count = 0;
    float3 previous = corners[3];
    previous = float3(m.x*previous.x + m.z*previous.z, previous.y, m.y*previous.x + m.w*previous.z);
    [unroll] for (uint i=0; i<4; ++i) {
        float3 p = corners[i];
        p = float3(m.x*p.x + m.z*p.z, p.y, m.y*p.x + m.w*p.z);
        if ((p.z > 0) != (previous.z > 0))
            clipped[count++] = unit_or(lerp(previous, p, previous.z / (previous.z-p.z)), 0);
        if (p.z > 0)
            clipped[count++] = unit_or(p, 0);
        previous = p;
    }
    if (count < 3) return 0;
    float sum = 0;
    float3 a = clipped[count-1];
    [loop] for (uint j=0; j<count; ++j) {
        float3 b = clipped[j];
        float3 axis = cross(a,b);
        float sine = length(axis);
        sum += axis.z * atan2(sine, clamp(dot(a,b),-1.0,1.0)) / max(sine,1e-12);
        a = b;
    }
    return saturate(abs(sum) / (2*PI));
}

float3 rect_radiance(LocalLight light, float3 position, float3 n, float3 v,
                     float3 f0, float3 diffuse_color, float roughness) {
    float3 u = light.half_width_range.xyz, h = light.half_height;
    float3 d = light.position_radius.xyz-position;
    if (dot(cross(u,h),d) >= 0) return 0;
    float3 nearest = d - u*clamp(dot(d,u)/dot(u,u),-1.0,1.0)
                       - h*clamp(dot(d,h)/dot(h,h),-1.0,1.0);
    float range = light.half_width_range.w;
    float radial = dot(nearest,nearest)/(range*range);
    if (radial >= 1) return 0;
    float window = 1-radial*radial;

    float nv = saturate(dot(n,v));
    float3 axis = abs(n.z)<0.9 ? float3(0,0,1) : float3(0,1,0);
    float3 t = unit_or(v-n*dot(n,v),normalize(cross(axis,n)));
    float3x3 basis = float3x3(t, cross(n,t), n);
    float3 corners[4] = {mul(basis,d-u-h),mul(basis,d+u-h),mul(basis,d+u+h),mul(basis,d-u+h)};
    float2 uv = float2(saturate(roughness),sqrt(1-nv))*31;
    uint2 cell = min((uint2)uv,30u);
    float2 f = uv-cell;
    uint index = cell.y*32u+cell.x;
    float4 m0,m1,m2,m3;
    float2 a0,a1,a2,a3;
    ltc_coefficients(index,m0,a0);
    ltc_coefficients(index+1,m1,a1);
    ltc_coefficients(index+32,m2,a2);
    ltc_coefficients(index+33,m3,a3);
    float4 m = lerp(lerp(m0,m1,f.x),lerp(m2,m3,f.x),f.y);
    float2 amplitude = lerp(lerp(a0,a1,f.x),lerp(a2,a3,f.x),f.y);
    float diffuse = rect_cosine_integral(corners,float4(1,0,0,1));
    float specular = rect_cosine_integral(corners,m);
    return light.color_intensity.rgb * window * window *
        (diffuse_color*(1-f0)*diffuse + (f0*amplitude.x + (1-f0)*amplitude.y)*specular);
}
