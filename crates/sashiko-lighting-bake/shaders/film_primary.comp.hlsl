// Immutable geometry and physical material records from the cooked scene.
ByteAddressBuffer scene : register(t0);
struct FilmRay { float3 origin; uint seed; float3 direction; uint reserved; };
StructuredBuffer<FilmRay> rays : register(t1);
#ifdef __spirv__
[[vk::ext_decorate(25)]]
#endif
RWStructuredBuffer<uint4> hits : register(u2);
RaytracingAccelerationStructure world : register(t3);

uint hash32(uint x) {
    x ^= x >> 16;
    x *= 0x7feb352d;
    x ^= x >> 15;
    x *= 0x846ca68b;
    return x ^ (x >> 16);
}

float maskTexel(uint texture, int2 p, int2 extent) {
    p = (p + extent) % extent;
    return (scene.Load(texture + 8 + 4 * (p.y * extent.x + p.x)) & 255) / 255.0;
}

float opacity(uint part, uint tri, float2 bary) {
    uint3 info = scene.Load3(part * 12);
    if (info.z == 0) return 1;
    uint3 ix = scene.Load3(info.y + tri * 12);
    float2 a = asfloat(scene.Load2(info.x + ix.x * 20 + 12));
    float2 b = asfloat(scene.Load2(info.x + ix.y * 20 + 12));
    float2 c = asfloat(scene.Load2(info.x + ix.z * 20 + 12));
    float2 uv = a * (1 - bary.x - bary.y) + b * bary.x + c * bary.y;
    int2 extent = int2(scene.Load2(info.z));
    float2 coord = frac(uv) * extent - 0.5;
    int2 base = int2(floor(coord));
    float2 f = frac(coord);
    return lerp(lerp(maskTexel(info.z, base, extent), maskTexel(info.z, base+int2(1,0), extent), f.x),
                lerp(maskTexel(info.z, base+int2(0,1), extent), maskTexel(info.z, base+1, extent), f.x), f.y);
}

[numthreads(64,1,1)]
void main(uint3 id : SV_DispatchThreadID) {
    FilmRay r = rays[id.x];
    if (r.reserved != 0) return;
    RayDesc ray;
    ray.Origin = r.origin;
    ray.Direction = r.direction;
    ray.TMin = 0;
    ray.TMax = 3.402823466e38;
    RayQuery<RAY_FLAG_NONE> q;
    q.TraceRayInline(world,RAY_FLAG_NONE,255,ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            float a = opacity(q.CandidateInstanceID(), q.CandidatePrimitiveIndex(), q.CandidateTriangleBarycentrics());
            float u = (hash32(r.seed ^ asuint(q.CandidateTriangleRayT())) >> 8) * (1.0/16777216.0);
            if (a > u) q.CommitNonOpaqueTriangleHit();
        }
    }
    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        hits[id.x] = uint4(asuint(q.CommittedRayT()), q.CommittedInstanceIndex(), q.CommittedInstanceID(), q.CommittedPrimitiveIndex());
    } else {
        hits[id.x] = uint4(0, 0xffffffff, 0xffffffff, 0xffffffff);
    }
}
