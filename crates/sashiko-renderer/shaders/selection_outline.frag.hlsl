// Training-yard post-process. Coordinates address the selection attachment at
// its native resolution. Outside the image is the unselected background.
Texture2D<float4> ids : register(t0);
struct Selection { uint id; };
#ifdef __spirv__
[[vk::push_constant]] Selection selection;
#else
ConstantBuffer<Selection> selection : register(b0);
#endif

bool selected(int2 p, uint2 size) {
    if (any(p < 0) || any(uint2(p) >= size)) return false;
    uint4 bytes = uint4(round(ids.Load(int3(p, 0)) * 255.0));
    return (bytes.x | (bytes.y << 8) | (bytes.z << 16) | (bytes.w << 24)) == selection.id;
}

float4 main(float4 pixel : SV_Position) : SV_Target0 {
    uint width, height;
    ids.GetDimensions(width, height);
    uint2 size = uint2(width, height);
    int2 p = int2(pixel.xy);
    if (selected(p, size)) return 0;
    bool border = selected(p + int2(-1,0), size) || selected(p + int2(1,0), size) ||
                  selected(p + int2(0,-1), size) || selected(p + int2(0,1), size);
    return border ? float4(1,0,1,1) : 0;
}
