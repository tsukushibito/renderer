use super::{ATLAS_DESC, AssignedPages, CAMERA_BYTES, PageRequests, TABLE_BYTES, layout};
use sashiko_renderer_core::{
    ImageFormat,
    capacity::Preparation,
    graph::{Binding, BindingResource, Graph, GraphError, ImageDesc, ImageHandle, ImageView},
    program::{
        BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups, ManagedProgram,
    },
};

const PASS: &str = "resolve virtual shadow visibility";
const IMAGE: &str = "virtual shadow visibility";

#[allow(unsafe_code)]
fn program() -> ManagedProgram {
    static DESC: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [8, 8, 1],
        bindings: &[
            layout(0, BindingType::Uniform, CAMERA_BYTES),
            layout(1, BindingType::SampledImage, 0),
            layout(2, BindingType::StorageRead, TABLE_BYTES),
            layout(3, BindingType::SampledDepth, 0),
            layout(
                4,
                BindingType::StorageImageWrite(ImageFormat::Rgba16Float),
                0,
            ),
            layout(5, BindingType::ComparisonSampler, 0),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Image2d {
                group: 0,
                binding: 4,
            },
            bytes: &[CAMERA_BYTES as u64, 0, TABLE_BYTES as u64, 0, 0, 0],
        },
        shaders: native_shader!("resolve"),
    };
    // SAFETY: each invocation owns a guarded output pixel. Depth loads use
    // source dimensions; level and page bounds precede every table load.
    // The shader verifies atlas extent and the encoded slot before indexing.
    unsafe { ManagedProgram::from_trusted_descriptor(&DESC) }
}

/// Reserve an optional depth-only inspection of the filtered sun visibility.
pub fn reserve_visibility(prep: &mut Preparation, extent: [u32; 2]) -> Result<(), GraphError> {
    prep.reserve_sampler(sashiko_renderer_core::program::SamplerDescriptor::ShadowCompare);
    prep.reserve_transient_image(IMAGE, ImageDesc::rgba16_float(extent[0], extent[1]))?;
    prep.reserve_dispatch(PASS, &program())
}

/// Return linear visibility for each receiver; sky and uncovered pixels are lit.
pub fn resolve_visibility(
    graph: &mut Graph,
    depth: ImageView,
    requests: PageRequests,
    pages: &AssignedPages,
    atlas: ImageHandle,
) -> Result<ImageHandle, GraphError> {
    let source = graph.image_desc(depth.image())?;
    if source.format != ImageFormat::Depth32Float || graph.image_desc(atlas)? != ATLAS_DESC {
        return Err(GraphError::invalid_input(
            "invalid virtual shadow resolve images",
        ));
    }
    let [w, h] = source
        .mip_extent(depth.mip_level())
        .ok_or_else(|| GraphError::invalid_input("invalid receiver depth mip"))?;
    let output = graph.image(IMAGE, ImageDesc::rgba16_float(w, h))?;
    let buffer = |buffer, size| BindingResource::Buffer {
        buffer,
        offset: 0,
        size,
    };
    let resources = [
        buffer(requests.camera, CAMERA_BYTES as u64),
        BindingResource::ImageView(depth),
        buffer(pages.table, TABLE_BYTES as u64),
        BindingResource::ImageView(graph.image_view(atlas, 0, 0)?),
        BindingResource::ImageView(graph.image_view(output, 0, 0)?),
        BindingResource::Sampler(sashiko_renderer_core::program::SamplerDescriptor::ShadowCompare),
    ];
    graph.dispatch(
        PASS,
        &program(),
        &std::array::from_fn::<_, 6, _>(|i| Binding {
            group: 0,
            binding: i as u32,
            resource: resources[i],
        }),
    )?;
    Ok(output)
}
