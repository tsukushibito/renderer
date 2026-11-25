use sashiko_renderer_core::{
    graph::{
        Binding, BindingResource, BufferDesc, CompiledGraph, GraphError, GraphPool, PassHandle,
    },
    program::ManagedProgram,
};

const BUFFER: BufferDesc = BufferDesc { size: 32 };

fn encode(pool: &mut GraphPool, value: u8) -> Result<(CompiledGraph, PassHandle), GraphError> {
    let mut graph = pool.begin()?;
    let input = graph.buffer("input", BUFFER)?;
    let output = graph.buffer("output", BUFFER)?;
    graph.upload_buffer("set input", input, 0, &[value; 16])?;
    let bindings = [(0, input), (1, output)].map(|(group, buffer)| Binding {
        group,
        binding: 0,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size: BUFFER.size,
        },
    });
    graph.dispatch("compute", &ManagedProgram::uniform_readback(), &bindings)?;
    let readback = graph.readback_buffer("result", output, 0, 16)?;
    Ok((graph.compile()?, readback))
}

#[test]
fn compile_without_graphics_or_device() -> Result<(), GraphError> {
    let mut pool = GraphPool::new(Default::default(), 1)?;
    let (graph, _) = encode(&mut pool, 7)?;
    assert_eq!(graph.ordered_pass_names().count(), 3);
    Ok(())
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn reverse_completion(
    config: sashiko_renderer_core::runtime::RuntimeConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use sashiko_renderer_core::runtime::{ResourceClass, Runtime, WorkingSet};
    let mut runtime = Runtime::new(
        config,
        Default::default(),
        WorkingSet {
            samplers: &[],
            resources: &[ResourceClass::Buffer {
                desc: BUFFER,
                count: 4,
            }],
            compute: &[ManagedProgram::uniform_readback()],
            raster: &[],
        },
    )?;
    let mut pool = GraphPool::new(Default::default(), 2)?;
    let mut submit = |value| -> Result<_, Box<dyn std::error::Error>> {
        let (compiled, readback) = encode(&mut pool, value)?;
        Ok((runtime.submit(&compiled)?.id(), readback, value))
    };
    let first = submit(7)?;
    let second = submit(11)?;
    for (id, readback, value) in [second, first] {
        let mut completed = runtime
            .wait_for_frame(id, std::time::Duration::from_secs(15))?
            .ok_or("compute timed out")?;
        let mut bytes = [0; 16];
        completed.readback_into(readback, &mut bytes)?;
        assert_eq!(bytes, [value; 16]);
    }
    Ok(())
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_compute() -> Result<(), Box<dyn std::error::Error>> {
    reverse_completion(sashiko_renderer_core::runtime::RuntimeConfig::Vulkan(
        Default::default(),
    ))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_compute() -> Result<(), Box<dyn std::error::Error>> {
    reverse_completion(sashiko_renderer_core::runtime::RuntimeConfig::D3D12(
        Default::default(),
    ))
}
