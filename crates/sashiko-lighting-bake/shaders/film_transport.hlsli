#include "film_sky.hlsli"

float3 linearTexel(uint map, int2 size, int2 p, bool srgb) {
    p = (p % size + size) % size;
    uint raw = scene.Load(map + 8 + 4 * (p.x + p.y * size.x));
    float3 value = uint3(raw & 255, (raw >> 8) & 255, (raw >> 16) & 255) / 255.0;
    if (srgb) {
        float3 nonlinear = pow((value + 0.055) / 1.055, 2.4);
        value = lerp(value / 12.92, nonlinear, step(0.04045, value));
    }
    return value;
}

float3 sampleMap(uint map, float2 uv, bool srgb) {
    int2 size = int2(scene.Load2(map));
    float2 xy = frac(uv) * size - 0.5;
    int2 p = int2(floor(xy));
    float2 f = frac(xy);
    return lerp(lerp(linearTexel(map, size, p, srgb), linearTexel(map, size, p + int2(1,0), srgb), f.x),
                lerp(linearTexel(map, size, p + int2(0,1), srgb), linearTexel(map, size, p + 1, srgb), f.x), f.y);
}

struct Hit {
    float3 p, normal, geometric;
    float3 reflection, transmission;
    float eta, roughness, internalReflectance;
    uint kind;
    bool entering;
};

Hit surface(inout RayQuery<RAY_FLAG_NONE> q, float3 origin, float3 direction) {
    uint part = partAddress(q.CommittedInstanceID());
    uint3 mesh = scene.Load3(part);
    uint3 vertices = mesh.x + vertexStride * scene.Load3(mesh.y + 12 * q.CommittedPrimitiveIndex());
    float2 bary = q.CommittedTriangleBarycentrics();
    float3 weights = float3(1 - bary.x - bary.y, bary);
    float3x4 model = q.CommittedObjectToWorld3x4();
    float3x3 inv = (float3x3)q.CommittedWorldToObject3x4();
    float3 p0 = asfloat(scene.Load3(vertices.x)), p1 = asfloat(scene.Load3(vertices.y)), p2 = asfloat(scene.Load3(vertices.z));
    float3 e1 = mul((float3x3)model, p1-p0), e2 = mul((float3x3)model, p2-p0);
    float2 uv0 = asfloat(scene.Load2(vertices.x + 12)), uv1 = asfloat(scene.Load2(vertices.y + 12)), uv2 = asfloat(scene.Load2(vertices.z + 12));
    float2 uv = weights.x * uv0 + weights.y * uv1 + weights.z * uv2;
    float3 n0 = asfloat(scene.Load3(vertices.x + 20)), n1 = asfloat(scene.Load3(vertices.y + 20)), n2 = asfloat(scene.Load3(vertices.z + 20));
    float3 n = weights.x*n0 + weights.y*n1 + weights.z*n2;
    Hit h;
    h.p = origin + q.CommittedRayT() * direction;
    float3 ng = normalize(mul(cross(p1-p0, p2-p0), inv));
    n = mul(n, inv);
    n = dot(n,n) > 0 ? normalize(n) : ng;
    if (dot(ng,n) < 0) ng = -ng;
    uint3 maps = scene.Load3(part+12);
    float2 a = uv1-uv0, b = uv2-uv0;
    float det = a.x*b.y - a.y*b.x;
    if (maps.z && abs(det) > 1e-10) {
        float3 dpdu = (b.y*e1 - a.y*e2) / det, dpdv = (a.x*e2 - b.x*e1) / det;
        // Project two neighbouring camera rays onto the tangent plane to obtain
        // a UV footprint even at secondary intersections of the film path.
        float3 eyeToHit = h.p - asfloat(film.Load3(32));
        float3 viewDir = normalize(eyeToHit);
        float3 up = asfloat(film.Load3(80)), right = normalize(cross(viewDir,up));
        up = cross(right,viewDir);
        float cosView = dot(viewDir,ng);
        float footprint = length(eyeToHit) * asfloat(film.Load(92));
        float2 duv = .0005;
        if (abs(cosView) > 1e-6) {
            float3 dx = footprint*(right - viewDir*(dot(right,ng)/cosView));
            float3 dy = footprint*(up - viewDir*(dot(up,ng)/cosView));
            float3 uvNormal = cross(dpdu,dpdv);
            float invArea2 = rcp(max(dot(uvNormal,uvNormal),1e-30));
            float3 gradU = cross(dpdv,uvNormal)*invArea2;
            float3 gradV = cross(uvNormal,dpdu)*invArea2;
            duv = .5*float2(abs(dot(dx,gradU))+abs(dot(dy,gradU)),abs(dot(dx,gradV))+abs(dot(dy,gradV)));
            duv = float2(duv.x>0 ? duv.x : .0005, duv.y>0 ? duv.y : .0005);
        }
        float height = sampleMap(maps.z,uv,false).x;
        float unit = asfloat(scene.Load(20));
        float dhdu = (sampleMap(maps.z,uv+float2(duv.x,0),false).x-height)*unit/duv.x;
        // Authored v increases upward. Its shading bitangent is built from the
        // smooth normal and u tangent in the source world's handedness.
        float dhdv = (sampleMap(maps.z,uv-float2(0,duv.y),false).x-height)*unit/duv.y;
        float3 bitangent = -cross(n,dpdu);
        float3 tangent = dpdu - n * dot(n,dpdu);
        float3 dndu = mul((b.y*(n1-n0) - a.y*(n2-n0))/det, inv);
        float3 dndv = mul((b.x*(n1-n0) - a.x*(n2-n0))/det, inv);
        float3 bumped = cross(tangent + n*dhdu + unit*height*dndu, bitangent + n*dhdv + unit*height*dndv);
        if (dot(bumped,bumped) > 0) n = normalize(bumped) * (dot(bumped,ng) < 0 ? -1 : 1);
    }
    h.entering = dot(ng,direction) < 0;
    h.geometric = h.entering ? ng : -ng;
    h.normal = dot(n,direction) < 0 ? n : -n;
    h.kind = scene.Load(part+24);
    h.eta = asfloat(scene.Load(part+28));
    h.roughness = asfloat(scene.Load(part+44));
    h.internalReflectance = asfloat(scene.Load(part+52));
    float scale = h.kind == 1 ? asfloat(scene.Load(part+48)) : 1;
    h.reflection = saturate(asfloat(scene.Load3(part+32)) * (maps.x ? sampleMap(maps.x,uv,true) : 1) * scale);
    h.transmission = h.kind == 1 ? saturate((maps.y ? sampleMap(maps.y,uv,true) : 0.25) * scale) : 0;
    return h;
}

float fresnel(float cosTheta, float eta) {
    float sin2 = (1-cosTheta*cosTheta)/(eta*eta);
    if (sin2 >= 1) return 1;
    float ct = sqrt(1-sin2);
    float2 num = float2(eta*cosTheta-ct, cosTheta-eta*ct);
    float2 den = float2(eta*cosTheta+ct, cosTheta+eta*ct);
    float2 r = num / max(den, 1e-10);
    return 0.5 * dot(r,r);
}

float3 oriented(float3 n, float azimuth, float cosine) {
    float3 tangent = normalize(cross(abs(n.z)<0.99 ? float3(0,0,1) : float3(1,0,0), n));
    return cosine*n + sqrt(saturate(1-cosine*cosine)) * (cos(azimuth)*tangent + sin(azimuth)*cross(n,tangent));
}

float positiveMax(float3 value) { return max(value.x,max(value.y,value.z)); }

// Return f * |n.wi| and the density of the complete BSDF sampling mixture.
float3 evaluate(Hit h, float3 wo, float3 wi, out float pdf) {
    float ci = dot(wi,h.normal), co = saturate(dot(wo,h.normal));
    bool reflection = ci > 0;
    pdf = 0;
    if (co <= 0) return 0;
    float r = positiveMax(h.reflection), t = positiveMax(h.transmission);
    float reflectProb = r / max(r+t,1e-10);
    float specProb = h.kind == 2 ? 0.25 : 0;
    pdf = (1-specProb) * (reflection ? reflectProb : 1-reflectProb) * abs(ci)/pi;
    float3 value = (reflection ? h.reflection : h.transmission) * abs(ci)/pi;
    if (h.kind == 2 && reflection) {
        float3 m = normalize(wo+wi);
        float cm = saturate(dot(m,h.normal)), wm = saturate(dot(wo,m));
        // PBRT remaps authored roughness to the square root before GGX evaluation.
        float alpha2 = max(1e-6,h.roughness);
        float denom = cm*cm*(alpha2-1)+1;
        float d = alpha2/(pi*denom*denom);
        float maskDenom = ci*sqrt(alpha2 + (1-alpha2)*co*co) + co*sqrt(alpha2 + (1-alpha2)*ci*ci);
        // Refraction contracts the external solid angle at the substrate.
        // Sum diffuse returns from the inside of the coat as a geometric series.
        value *= (1-fresnel(co,h.eta))*(1-fresnel(ci,h.eta)) /
                 (h.eta*h.eta * max(1-h.reflection*h.internalReflectance, 1e-6));
        if (h.roughness > 0 && wm > 0) {
            value += fresnel(wm,h.eta)*d*ci / max(2*maskDenom,1e-10);
            pdf += specProb*d*cm/(4*wm);
        }
    }
    return value;
}

float3 offsetPoint(Hit h, float3 wi) {
    float epsilon = 2e-6 * max(1,positiveMax(abs(h.p)));
    return h.p + (dot(wi,h.geometric) > 0 ? epsilon : -epsilon)*h.geometric;
}

float powerWeight(float a, float b) {
    return a*a / max(a*a+b*b, 1e-30);
}

float3 integrate(FilmRay camera) {
    uint rng = camera.seed;
    float3 o = camera.origin, dir = camera.direction, throughput = 1, sum = 0;
    float lastPdf = 0;
    uint lighting = scene.Load(24);
    bool directOnly = lighting == 1 || lighting == 3;
    for (uint bounce = 0; bounce <= 5; ++bounce) {
        RayQuery<RAY_FLAG_NONE> q;
        bool escaped = lighting == 3 && bounce == 1;
        if (!escaped) {
            traverse(q,o,dir,hash32(rng));
            escaped = q.CommittedStatus() != COMMITTED_TRIANGLE_HIT;
        }
        if (escaped) {
            float2 uv = lightUV(dir);
            if (lighting != 2 || bounce >= 2)
                sum += throughput * skyColor(uv) * (lastPdf > 0 ? powerWeight(lastPdf,skyPdf(uv)) : 1);
            break;
        }
        if (bounce == 5 || (directOnly && bounce == 1)) break;
        Hit h = surface(q,o,dir);
        float3 wo = -dir;
        lastPdf = 0;
        if (h.kind == 4 || h.kind == 3) {
            float eta = h.entering ? h.eta : 1/h.eta;
            if (h.kind == 3 || randomUnit(rng) < fresnel(saturate(dot(wo,h.normal)),eta)) {
                dir = reflect(-wo,h.normal);
                if (h.kind == 3) throughput *= h.reflection;
            } else {
                dir = refract(-wo,h.normal,1/eta);
                throughput /= eta*eta;
            }
        } else {
            float2 uv;
            float lightPdf, bsdfPdf;
            float3 wi = sampleSky(rng,uv,lightPdf);
            float3 value = evaluate(h,wo,wi,bsdfPdf);
            if ((lighting != 2 || bounce != 0) && lightPdf > 0 && any(value > 0)) {
                bool visible = lighting == 3;
                if (!visible) {
                    RayQuery<RAY_FLAG_NONE> shadow;
                    traverse(shadow,offsetPoint(h,wi),wi,hash32(rng ^ 0xb5297a4d));
                    visible = shadow.CommittedStatus() != COMMITTED_TRIANGLE_HIT;
                }
                if (visible)
                    sum += throughput * value * skyColor(uv) * (powerWeight(lightPdf,bsdfPdf)/lightPdf);
            }
            bool coat = h.kind == 2 && randomUnit(rng) < 0.25;
            float u = randomUnit(rng), phi = 2*pi*randomUnit(rng);
            if (coat && h.roughness == 0) {
                dir = reflect(-wo,h.normal);
                throughput *= 4 * fresnel(saturate(dot(wo,h.normal)),h.eta);
            } else {
                if (coat) {
                    float cm = sqrt((1-u)/(1+(max(1e-6,h.roughness)-1)*u));
                    dir = reflect(-wo,oriented(h.normal,phi,cm));
                    if (dot(dir,h.normal) <= 0) break;
                } else {
                    float r = positiveMax(h.reflection), t = positiveMax(h.transmission);
                    dir = oriented(randomUnit(rng)*(r+t) < r ? h.normal : -h.normal, phi, sqrt(u));
                }
                value = evaluate(h,wo,dir,lastPdf);
                if (lastPdf <= 0) break;
                throughput *= value/lastPdf;
            }
        }
        if (!any(throughput > 0)) break;
        o = offsetPoint(h,dir);
    }
    return sum;
}

#include "film_camera.hlsli"
