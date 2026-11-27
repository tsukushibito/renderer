float2 virtual_page_texel(float2 light_uv, uint level, int2 origin) {
    return light_uv * (3200.0 / float(1u << level)) - float2(origin) * 128.0;
}

uint virtual_receiver_level(float distance) {
    uint level = 0;
    while (level < 7 && distance >= 1.28 * float(1u << level)) ++level;
    return level;
}
