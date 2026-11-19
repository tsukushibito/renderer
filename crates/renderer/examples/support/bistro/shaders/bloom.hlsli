Texture2D<float4> source_image : register(t0, space0);

#ifdef __spirv__
[[vk::image_format("rgba16f")]]
[[vk::ext_decorate(25)]] // SPIR-V NonReadable. GetDimensions only queries the extent.
#endif
RWTexture2D<float4> destination_image : register(u0, space1);

int2 texture_extent(Texture2D<float4> image) {
    uint2 extent;
    uint mip_count;
    image.GetDimensions(0, extent.x, extent.y, mip_count);
    return int2(extent);
}

int2 destination_extent() {
    uint2 extent;
    destination_image.GetDimensions(extent.x, extent.y);
    return int2(extent);
}
