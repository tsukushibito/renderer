// One group integrates a world-space receiver. Scene records are immutable
// RGBA32F texels. All indirect loads are clamped to the actual view extent.
#ifdef BAKE_RAY_QUERY
StructuredBuffer<float4> scene : register(t0);
RaytracingAccelerationStructure world : register(t3);
#else
Texture2D<float4> scene : register(t0);
#endif
struct Receiver {
    float4 p;
    float4 normal;
}; // zero normal = volume probe
StructuredBuffer<Receiver> jobs : register(t1);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<float4> result : register(u2);
static const float PI = 3.14159265359;
struct Table {
    uint rect_base, rect_count, point_base, point_count;
    uint width, last, node_base, nodes, tris, mats, tex, texels;
};
uint number(float v) {
    return (uint)(v >= 0.0 ? min(v, 16777215.0) : 0.0);
}
float4 fetch(Table t, uint i) {
    i = min(i, t.last);
#ifdef BAKE_RAY_QUERY
    return scene[i];
#else
    return scene.Load(int3(i % t.width, i / t.width, 0));
#endif
}
Table table() {
    Table t;
    uint h;
#ifdef BAKE_RAY_QUERY
    scene.GetDimensions(t.width, h);
    h = 1;
#else
    scene.GetDimensions(t.width, h);
#endif
    t.last = t.width * h - 1;
    float4 counts = fetch(t, 0);
    t.nodes = number(counts.x);
    t.rect_base = 2 + 3 * clamp(number(fetch(t,1).y),1u,16u);
    t.rect_count = min(number(fetch(t,1).z), t.last / 4u);
    t.point_base = t.rect_base + 4 * t.rect_count;
    t.point_count = min(number(fetch(t,1).w), t.last / 2u);
    t.node_base = t.point_base + 2 * t.point_count;
#ifdef BAKE_RAY_QUERY
    t.tris = t.node_base;
#else
    t.tris = t.node_base + 3 * t.nodes;
#endif
    t.mats = t.tris + 4 * number(counts.y);
    t.tex = t.mats + 3 * number(counts.z);
    t.texels = t.tex + number(counts.w);
    return t;
}
float4 pixel(Table t, uint base, int2 size, int2 p) {
    int2 q = ((p % size) + size) % size;
    return fetch(t, base + q.y * size.x + q.x);
}
float4 texture_color(Table t, float index, float2 uv) {
    if (index < 0)
        return 1;
    float4 rec = fetch(t, t.tex + number(index));
    int2 size = clamp(int2(number(rec.x), number(rec.y)), 1, 16384);
    uint base = t.texels + number(rec.z) + 65536 * number(rec.w);
    float2 p = frac(uv) * size - 0.5;
    int2 lo = (int2)floor(p);
    float2 f = frac(p);
    return lerp(lerp(pixel(t, base, size, lo), pixel(t, base, size, lo + int2(1, 0)), f.x),
                lerp(pixel(t, base, size, lo + int2(0, 1)), pixel(t, base, size, lo + 1), f.x),
                f.y);
}
struct Hit {
    float dist;
    float3 n, diffuse, emission;
};
#ifdef BAKE_RAY_QUERY
// Geometry 0 is opaque, geometry 1 uses the same filtered alpha test as transport.
uint rt_triangle(Table t, uint geometry, uint primitive) {
    return t.tris + 4 * (primitive + (geometry == 0 ? 0 : t.nodes));
}
float2 rt_uv(float4 a, float4 b, float4 c, float4 m, float2 bary) {
    float3 w = float3(1 - bary.x - bary.y, bary);
    return float2(dot(w, float3(a.w, b.w, c.w)), dot(w, m.xyz));
}
bool trace_ray(Table t, float3 origin, float3 direction, float limit, bool occlusion, out Hit hit) {
    hit = (Hit)0;
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = direction;
    // Match the software path's strictly positive intersection distance.
    ray.TMin = asfloat(0x00800000);
    ray.TMax = limit;
    RayQuery<RAY_FLAG_NONE> query;
    query.TraceRayInline(world, occlusion ? RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH : RAY_FLAG_NONE, 255, ray);
    while (query.Proceed()) {
        if (query.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            uint at = rt_triangle(t, query.CandidateGeometryIndex(), query.CandidatePrimitiveIndex());
            float4 m = fetch(t, at + 3);
            uint mat = t.mats + 3 * number(m.w);
            float2 uv = rt_uv(fetch(t, at), fetch(t, at + 1), fetch(t, at + 2), m, query.CandidateTriangleBarycentrics());
            if (fetch(t, mat + 2).x * texture_color(t, fetch(t, mat).w, uv).a >= fetch(t, mat + 1).w)
                query.CommitNonOpaqueTriangleHit();
        }
    }
    if (query.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
        return false;
    if (occlusion)
        return true;
    uint at = rt_triangle(t, query.CommittedGeometryIndex(), query.CommittedPrimitiveIndex());
    float4 a = fetch(t, at), b = fetch(t, at + 1), c = fetch(t, at + 2), m = fetch(t, at + 3);
    float2 uv = rt_uv(a, b, c, m, query.CommittedTriangleBarycentrics());
    uint mat = t.mats + 3 * number(m.w);
    float4 albedo = fetch(t, mat), props = fetch(t, mat + 2), emission = fetch(t, mat + 1);
    float3 normal = cross(b.xyz - a.xyz, c.xyz - a.xyz);
    hit.n = normalize(normal) * sign(-dot(normal, direction));
    hit.dist = query.CommittedRayT();
    hit.diffuse = albedo.rgb * saturate(texture_color(t, albedo.w, uv).rgb) *
                  (1 - props.y * saturate(texture_color(t, props.z, uv).b));
    hit.emission = emission.rgb * texture_color(t, props.w, uv).rgb;
    return true;
}
#else
bool trace_ray(Table t, float3 o, float3 d, float limit, bool occlusion, out Hit hit) {
    hit = (Hit)0;
    bool found = false;
    uint i = 0;
    [loop] while (i < t.nodes) {
        uint p = t.node_base + 3 * i;
        float4 a = fetch(t, p), b = fetch(t, p + 1), tail = fetch(t, p + 2);
        float3 inv = 1.0 / select(d == 0.0, 1.0, d);
        float3 low = (a.xyz - o) * inv, high = (b.xyz - o) * inv;
        float3 near_t = select(d == 0.0, 0.0, min(low, high));
        float3 far_t = select(d == 0.0, limit, max(low, high));
        if (any((d == 0.0) & ((o < a.xyz) | (o > b.xyz))) ||
            max(max(near_t.x, near_t.y), max(near_t.z, 0.0)) >
                min(min(far_t.x, far_t.y), min(far_t.z, limit))) {
            i = max(i + 1, number(tail.x));
            continue;
        }
        ++i;
        uint count = min(number(b.w), 8u);
        [loop] for (uint j = 0; j < count; j++) {
            uint tri = t.tris + 4 * (number(a.w) + j);
            float4 v0 = fetch(t, tri), v1 = fetch(t, tri + 1), v2 = fetch(t, tri + 2);
            float3 e = v1.xyz - v0.xyz, f = v2.xyz - v0.xyz;
            float3 h = cross(d, f), delta = o - v0.xyz;
            float det = dot(e, h);
            if (det == 0.0)
                continue;
            float u = dot(delta, h) / det;
            float3 q = cross(delta, e);
            float v = dot(d, q) / det, dist = dot(f, q) / det;
            if (!(u >= 0.0 && v >= 0.0 && u + v <= 1.0 && dist > 0.0 && dist < limit))
                continue;
            float4 uvmat = fetch(t, tri + 3);
            uint mat = t.mats + 3 * number(uvmat.w);
            float4 emission = fetch(t, mat + 1);
            if (occlusion && emission.w < 0)
                return true;
            float4 albedo = fetch(t, mat);
            float3 bary = float3(1.0 - u - v, u, v);
            float2 uv = float2(dot(bary, float3(v0.w, v1.w, v2.w)), dot(bary, uvmat.xyz));
            float4 properties = fetch(t, mat + 2);
            float4 color = texture_color(t, albedo.w, uv);
            if (properties.x * color.a < emission.w)
                continue;
            if (occlusion)
                return true;
            limit = dist;
            found = true;
            hit.dist = dist;
            hit.n = normalize(cross(e, f)) * sign(det);
            hit.diffuse = albedo.rgb * saturate(color.rgb) *
                          (1 - properties.y * saturate(texture_color(t, properties.z, uv).b));
            hit.emission = emission.rgb * texture_color(t, properties.w, uv).rgb;
        }
    }
    return found;
}
#endif
float random(inout uint seed) {
    seed = seed * 747796405u + 2891336453u;
    uint bits = ((seed >> ((seed >> 28) + 4)) ^ seed) * 277803737u;
    return ((bits ^ (bits >> 22)) >> 8) / 16777216.0;
}
float3 offset_ray(float3 p, float3 n) {
    float3 a = abs(p);
    return p + n * (4.76837158203125e-7 * max(1.0, max(a.x, max(a.y, a.z))));
}
float3 hemisphere(float3 n, float u, float v) {
    float3 tangent = normalize(cross(abs(n.z) < 0.9 ? float3(0, 0, 1) : float3(0, 1, 0), n));
    float s, c;
    sincos(2 * PI * v, s, c);
    return sqrt(u) * (c * tangent + s * cross(n, tangent)) + sqrt(saturate(1 - u)) * n;
}
float3 sky_radiance(Table t, float3 d, float4 sky, float yaw) {
    if (sky.w < 0.0) return sky.rgb;
    float height = max(1.0,fetch(t,t.tex+number(sky.w)).y);
    float longitude = dot(d.xz,d.xz) > 0.0 ? atan2(d.z,d.x) : 0.0;
    float u = 0.5 + (longitude+yaw)/(2.0*PI);
    float v = clamp(acos(clamp(d.y,-1.0,1.0))/PI,0.5/height,1.0-0.5/height);
    return sky.rgb * texture_color(t,sky.w,float2(u,v)).rgb;
}
float3 local_illuminance(Table t, float3 o, float3 n, uint seed) {
    float3 sum = 0;
    [loop] for (uint i=0; i<t.point_count; ++i) {
        uint base = t.point_base + 2*i;
        float4 emitter = fetch(t,base);
        float3 delta = emitter.xyz-o;
        float d2 = dot(delta,delta);
        float radial = d2/(emitter.w*emitter.w);
        if (radial >= 1) continue;
        float dist = sqrt(d2);
        float3 l = d2 > 1e-20 ? delta/dist : n;
        float cosine = max(dot(n,l),0);
        Hit blocker;
        if (cosine > 0 && !trace_ray(t,o,l,dist,true,blocker)) {
            float window = 1-radial*radial;
            sum += fetch(t,base+1).rgb * (cosine*window*window/max(d2,0.01));
        }
    }
    [loop] for (uint i=0; i<t.rect_count; ++i) {
        uint base = t.rect_base + 4*i;
        float4 center_range = fetch(t,base);
        float4 u = fetch(t,base+1);
        float3 v = fetch(t,base+2).xyz;
        float3 center = center_range.xyz-o;
        float3 nearest = center - u.xyz*clamp(dot(center,u.xyz)/dot(u.xyz,u.xyz),-1.0,1.0)
                                - v*clamp(dot(center,v)/dot(v,v),-1.0,1.0);
        float radial = dot(nearest,nearest)/(center_range.w*center_range.w);
        if (radial >= 1) continue;
        float x = 2*random(seed)-1;
        float y = 2*random(seed)-1;
        float3 delta = center + x*u.xyz + y*v;
        float d2 = dot(delta,delta);
        if (d2 <= 1e-12) continue;
        float dist = sqrt(d2);
        float3 l = delta/dist;
        float cosine = max(dot(l,n),0)*max(dot(-l,normalize(cross(u.xyz,v))),0);
        Hit blocker;
        if (cosine > 0 && !trace_ray(t,o,l,dist,true,blocker)) {
            float window = 1-radial*radial;
            sum += fetch(t,base+3).rgb*(u.w*cosine/d2*window*window);
        }
    }
    return sum;
}

float3 incoming(Table t, float3 o, float3 d, uint scenario, uint seed) {
    float4 sky = fetch(t, 2 + 3 * scenario);
    float yaw = fetch(t, 3 + 3 * scenario).w;
    float3 sun = fetch(t, 3 + 3 * scenario).rgb;
    float3 light = fetch(t, 4 + 3 * scenario).xyz;
    float3 val = 0, weight = 1;
    Hit hit;
    [loop] for (uint bounce = 0; bounce < 2; bounce++) {
        if (!trace_ray(t, o, d, 3.402823e38, false, hit))
            return val + weight * sky_radiance(t,d,sky,yaw);
        o = offset_ray(o + d * hit.dist, hit.n);
        float cosine = max(dot(hit.n, light), 0);
        Hit blocker;
        if (cosine > 0 && trace_ray(t, o, light, 3.402823e38, true, blocker))
            cosine = 0;
        val += weight * (hit.emission + hit.diffuse * (sun * cosine + local_illuminance(t,o,hit.n,seed ^ 0x9e3779b9u)) / PI);
        weight *= hit.diffuse;
        float u = random(seed);
        float v = random(seed);
        d = hemisphere(hit.n, u, v);
    }
    return val + (trace_ray(t, o, d, 3.402823e38, true, hit) ? 0 : weight * sky_radiance(t,d,sky,yaw));
}
#ifdef BAKE_SURFACE
static const uint COEFFICIENTS = 1;
#elif defined(BAKE_L2)
static const uint COEFFICIENTS = 9;
#else
static const uint COEFFICIENTS = 4;
#endif
// Two scenario slots are always initialized, including the last odd key.
static const uint TERMS = 2 * COEFFICIENTS;
#ifdef BAKE_SURFACE
static const uint RESPONSE_TEXELS = TERMS;
static const uint RESPONSE_OFFSET = 0;
#else
static const uint RESPONSE_TEXELS = 2 + TERMS;
static const uint RESPONSE_OFFSET = 2;
#endif
groupshared float3 sums[32][TERMS];
[numthreads(32, 1, 1)] void main(uint3 group
                                 : SV_GroupID, uint lane
                                 : SV_GroupIndex) {
    Table t = table();
    uint job = group.x;
    Receiver rec = jobs[job];
    float3 origin = rec.p.xyz;
#ifdef BAKE_SURFACE
    bool volume = false;
#else
    bool volume = all(rec.normal.xyz == 0.0);
#endif
    if (!volume)
        origin = offset_ray(origin, rec.normal.xyz);
    uint samples = clamp(number(fetch(t, 1).x), 1u, 4096u);
    uint count = clamp(number(fetch(t,1).y),1u,16u);
    uint first = min(number(rec.normal.w), count-1u);
    uint scenarios = min(count-first,2u);
    [unroll] for (uint k = 0; k < TERMS; k++) sums[lane][k] = 0;
    [loop] for (uint j = lane; j < samples; j += 32) {
        float u = (j + 0.5) / samples, v = reversebits(j) * 2.3283064365386963e-10;
        float z = 1 - 2 * u, r = sqrt(saturate(1 - z * z));
        float s, c;
        sincos(2 * PI * v, s, c);
        float3 d = volume ? float3(r * c, z, r * s) : hemisphere(rec.normal.xyz, u, v);
        [loop] for (uint side = 0; side < scenarios; side++) {
            float3 l = incoming(t, origin, d, first+side, j) * (PI / samples);
            uint base = COEFFICIENTS * side;
            sums[lane][base] += l;
#ifndef BAKE_SURFACE
            [unroll] for (uint axis = 0; axis < 3; axis++) sums[lane][base + axis + 1] +=
                l * (2 * d[axis]);
#endif
#ifdef BAKE_L2
            sums[lane][base + 4] += l * (3.75 * d.x * d.y);
            sums[lane][base + 5] += l * (3.75 * d.y * d.z);
            sums[lane][base + 6] += l * (3.75 * d.x * d.z);
            sums[lane][base + 7] += l * (0.9375 * (d.x * d.x - d.y * d.y));
            sums[lane][base + 8] += l * (0.3125 * (3 * d.z * d.z - 1));
#endif
        }
    }
    GroupMemoryBarrierWithGroupSync();
    if (lane < TERMS) {
        float3 total = 0;
        [unroll] for (uint l = 0; l < 32; l++) total += sums[l][lane];
        result[RESPONSE_TEXELS * job + RESPONSE_OFFSET + lane] = float4(total, 0);
    }
#ifndef BAKE_SURFACE
    if (lane < 2) {
        float3 distances = rec.p.w;
        if (volume && first == 0u) {
            [unroll] for (uint axis = 0; axis < 3; axis++) {
                float3 d = 0;
                d[axis] = lane == 0 ? -1 : 1;
                Hit hit;
                if (trace_ray(t, origin, d, rec.p.w, false, hit))
                    distances[axis] = hit.dist;
            }
        }
        result[RESPONSE_TEXELS * job + lane] = float4(distances, 0);
    }
#endif
}
