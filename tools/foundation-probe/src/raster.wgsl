struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var output: VertexOutput;
    output.position = vec4(positions[vertex], 0.25, 1.0);
    output.color = select(vec4(1.0, 0.0, 0.0, 1.0), vec4(0.0, 0.0, 1.0, 1.0), instance == 1u);
    return output;
}

@fragment
fn fs(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
