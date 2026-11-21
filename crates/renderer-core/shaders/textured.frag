#version 450 core

layout(location = 0) in vec4 vertex_color;
layout(location = 1) in vec2 sample_uv;
layout(location = 0) out vec4 output_color;

layout(set = 1, binding = 0) uniform texture2D source_image;
layout(set = 1, binding = 1) uniform sampler source_sampler;

void main() {
    // An explicit LOD and a single-mip 2D View bound with NearestClamp keep
    // sampling inside that View. Arbitrary UV inputs require its whole mip to
    // be initialized; this contract does not promise attachment coverage.
    output_color = textureLod(sampler2D(source_image, source_sampler), sample_uv, 0.0)
        * vertex_color;
}
