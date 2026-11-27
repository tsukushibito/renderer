StructuredBuffer<uint4> slots : register(t0);

float4 main(float2 corner : TEXCOORD0, uint instance_id : SV_InstanceID) : SV_Position {
    uint slot = min(instance_id, 1023u);
    if (slots[slot].w == 0u) return float4(2.0, 2.0, 1.0, 1.0);
    float2 xy = (uint2(slot % 32, slot / 32) + corner) / 16.0 - 1.0;
#ifndef __spirv__
    xy.y = -xy.y;
#endif
    return float4(xy, 1.0, 1.0);
}
