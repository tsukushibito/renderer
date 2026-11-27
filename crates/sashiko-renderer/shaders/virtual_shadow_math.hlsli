// Nine bilinear PCF taps at offsets -1..1 reach two texels past the interior.
static const uint VSM_GUTTER = 2;
static const uint VSM_TILE = 128 + 2 * VSM_GUTTER;
static const uint VSM_COLUMNS = 4096 / VSM_TILE;
static const uint VSM_CAPACITY = VSM_COLUMNS * VSM_COLUMNS;
uint2 virtual_tile_origin(uint slot) {
    return uint2(slot % VSM_COLUMNS, slot / VSM_COLUMNS) * VSM_TILE;
}

float2 virtual_page_texel(float2 light_uv, uint level, int2 origin) {
    return light_uv * (3200.0 / float(1u << level)) - float2(origin) * 128.0;
}

uint virtual_receiver_level(float distance) {
    uint level = 0;
    while (level < 7 && distance >= 1.28 * float(1u << level)) ++level;
    return level;
}
