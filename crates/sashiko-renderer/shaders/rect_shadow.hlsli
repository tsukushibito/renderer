// Five perspective faces in emitter-local coordinates, packed row-major.
Texture2D<float> rect_shadow_atlas : register(t5, space3);
static const uint RECT_FACE_SIZE = 512;

uint rect_face(float3 p) {
    float3 a = abs(p);
    if (p.z >= max(a.x, a.y)) return 0;
    if (a.x >= a.y) return p.x >= 0 ? 1 : 2;
    return p.y >= 0 ? 3 : 4;
}

float3 face_coords(uint face, float3 p) {
    if (face == 1) return float3(-p.z, p.y, p.x);
    if (face == 2) return float3(p.z, p.y, -p.x);
    if (face == 3) return float3(p.x, -p.z, p.y);
    if (face == 4) return float3(p.x, p.z, -p.y);
    return p;
}

float3 from_face(uint face, float3 p) {
    if (face == 1) return float3(p.z, p.y, -p.x);
    if (face == 2) return float3(-p.z, p.y, p.x);
    if (face == 3) return float3(p.x, p.z, -p.y);
    if (face == 4) return float3(p.x, -p.z, p.y);
    return p;
}

float rect_visibility(LocalLight light, float3 position) {
    uint width, height;
    rect_shadow_atlas.GetDimensions(width, height);
    uint columns = width / RECT_FACE_SIZE;
    uint slots = columns * (height / RECT_FACE_SIZE) / 5u;
    if (!(light.shadow_slot >= 1 && light.shadow_slot <= slots)) return 1;
    uint first = (uint(light.shadow_slot) - 1u) * 5u;
    float3 u = normalize(light.half_width_range.xyz), v = normalize(light.half_height);
    float3 delta = position - light.position_radius.xyz;
    float3 p = float3(dot(delta,u), dot(delta,v), dot(delta,cross(u,v)));
    uint face = rect_face(p);
    float3 q = face_coords(face,p);
    float far_z = light.position_radius.w;
    float near_z = min(0.01, far_z * 0.0001);
    if (q.z <= near_z || p.z <= 0) return 1;
    float2 uv = q.xy / q.z;
    float visibility = 0;
    [unroll] for (int y = -1; y <= 1; ++y) {
        [unroll] for (int x = -1; x <= 1; ++x) {
            float3 ray = from_face(face, float3(uv + float2(x,y)*(2.0/RECT_FACE_SIZE),1));
            uint tap_face = rect_face(ray);
            float3 tap = face_coords(tap_face,ray);
            uint2 pixel = min(uint2(saturate(tap.xy/tap.z*0.5+0.5)*RECT_FACE_SIZE), RECT_FACE_SIZE-1u);
            uint tile = first + tap_face;
            pixel += uint2(tile % columns,tile / columns)*RECT_FACE_SIZE;
            float z = max(near_z, face_coords(tap_face,p).z*(1.0-2.0/RECT_FACE_SIZE));
            float depth = far_z/(far_z-near_z)*(1-near_z/z);
            visibility += depth <= rect_shadow_atlas.Load(int3(pixel,0)) ? 1 : 0;
        }
    }
    return visibility / 9.0;
}
