// Uniform area on this square is uniform solid angle on the sphere.
static const float pi = 3.141592653589793;

float randomUnit(inout uint state) {
    state += 0x9e3779b9;
    return (hash32(state) >> 8) * (1.0 / 16777216.0);
}

float3 transformDirection(uint address, float3 d) {
    return normalize(float3(dot(asfloat(scene.Load3(address)), d),
                            dot(asfloat(scene.Load3(address+16)), d),
                            dot(asfloat(scene.Load3(address+32)), d)));
}

float2 lightUV(float3 worldDirection) {
    float3 d = transformDirection(32, worldDirection);
    float r = sqrt(saturate(1 - abs(d.z)));
    float a = any(d.xy != 0) ? atan2(abs(d.y), abs(d.x)) * (2/pi) : 0;
    float2 uv = r * float2(1-a, a);
    if (d.z < 0) uv = 1 - uv.yx;
    return .5 + .5 * uv * float2(d.x < 0 ? -1 : 1, d.y < 0 ? -1 : 1);
}

float3 skyColor(float2 uv) {
    uint2 image = scene.Load2(0);
    uint2 p = min(uint2(uv * image.y), image.y - 1);
    uint2 rgb = scene.Load2(image.x + 8*(p.y*image.y + p.x));
    return float3(f16tof32(rgb.x & 65535), f16tof32(rgb.x >> 16), f16tof32(rgb.y & 65535))
        * asfloat(scene.Load(16));
}

float cellMass(uint address, uint i) {
    float2 edge = asfloat(scene.Load2(address + 4*i));
    return edge.y - edge.x;
}

uint chooseCell(uint address, uint count, float u, out float offset) {
    uint lo = 0, hi = count;
    while (lo+1 < hi) {
        uint mid = (lo+hi)/2;
        if (asfloat(scene.Load(address + 4*mid)) <= u) lo = mid;
        else hi = mid;
    }
    float2 edge = asfloat(scene.Load2(address + 4*lo));
    offset = min((u-edge.x)/(edge.y-edge.x), asfloat(0x3f7fffff));
    return lo;
}

float skyPdf(float2 uv) {
    uint2 table = scene.Load2(8);
    uint n = table.y;
    uint2 i = min(uint2(uv*n), n-1);
    return cellMass(table.x, i.y) * cellMass(table.x + 4*(n+1)*(1+i.y), i.x) * (n*n/(4*pi));
}

float3 sampleSky(inout uint rng, out float2 uv, out float pdf) {
    uint2 table = scene.Load2(8);
    uint n = table.y;
    float dy, dx;
    uint y = chooseCell(table.x, n, randomUnit(rng), dy);
    uint row = table.x + 4*(n+1)*(y+1);
    uint x = chooseCell(row, n, randomUnit(rng), dx);
    pdf = cellMass(table.x, y) * cellMass(row, x) * (n*n/(4*pi));
    uv = (float2(x,y) + float2(dx,dy))/n;
    float2 p = 2*uv - 1;
    float diagonal = 1-abs(p.x)-abs(p.y);
    float r = 1-abs(diagonal);
    float phi = r > 0 ? (pi/4)*(1 + (abs(p.y)-abs(p.x))/r) : 0;
    float radial = r*sqrt(max(0, 2-r*r));
    return transformDirection(80, float3(radial*cos(phi)*(p.x<0?-1:1),
                                         radial*sin(phi)*(p.y<0?-1:1),
                                         (1-r*r)*(diagonal<0?-1:1)));
}
