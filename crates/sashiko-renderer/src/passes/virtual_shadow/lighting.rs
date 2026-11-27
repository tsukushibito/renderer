use super::{ATLAS_DESC, AssignedPages, PageRequests, TABLE_BYTES, layout};
use sashiko_renderer_core::{
    ImageFormat,
    capacity::Preparation,
    graph::{Binding, BindingResource, Graph, GraphError, ImageDesc, ImageHandle, ImageView},
    program::{
        BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups, ManagedProgram,
    },
};

const PASS: &str = "pack virtual shadow table";
const IMAGE: &str = "virtual shadow table texture";
const DESC: ImageDesc = ImageDesc::rgba16_float(32, 1024);

#[allow(unsafe_code)]
fn pack_program() -> ManagedProgram {
    static PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [8, 8, 1],
        bindings: &[
            layout(0, BindingType::StorageRead, TABLE_BYTES),
            layout(
                1,
                BindingType::StorageImageWrite(ImageFormat::Rgba16Float),
                0,
            ),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Image2d {
                group: 0,
                binding: 1,
            },
            bytes: &[TABLE_BYTES as u64, 0],
        },
        shaders: native_shader!("pack_table"),
    };
    // SAFETY: output dimensions guard each invocation. The source load is
    // separately bounded to 32 * 1024 uint4s; every output pixel is written.
    unsafe { ManagedProgram::from_trusted_descriptor(&PROGRAM) }
}

pub fn reserve_lighting(prep: &mut Preparation) -> Result<(), GraphError> {
    prep.reserve_transient_image(IMAGE, DESC)?;
    prep.reserve_dispatch(PASS, &pack_program())
}

/// PBR light state for opaque, masked and transparent receivers in this graph.
#[derive(Clone, Copy, Debug)]
pub struct VirtualSun {
    pub(crate) atlas: ImageView,
    pub(crate) table: ImageView,
    projection: [[u32; 4]; 12],
    distance: f32,
}

impl VirtualSun {
    pub(crate) fn camera_bytes(
        &self,
        base: &[u8; crate::passes::pbr::CAMERA_BYTES],
    ) -> [u8; crate::passes::pbr::CAMERA_BYTES] {
        let mut result = *base;
        for (word, value) in result[128..320]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(self.projection.as_flattened())
        {
            *word = value.to_le_bytes();
        }
        result[396..400].copy_from_slice(&(-self.distance).to_le_bytes());
        result
    }
}

pub fn lighting(
    graph: &mut Graph,
    requests: &PageRequests,
    pages: &AssignedPages,
    atlas: ImageHandle,
) -> Result<VirtualSun, GraphError> {
    if graph.image_desc(atlas)? != ATLAS_DESC {
        return Err(GraphError::invalid_input(
            "invalid virtual shadow lighting atlas",
        ));
    }
    let image = graph.image(IMAGE, DESC)?;
    let table = graph.image_view(image, 0, 0)?;
    graph.dispatch(
        PASS,
        &pack_program(),
        &[
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: pages.table,
                    offset: 0,
                    size: TABLE_BYTES as u64,
                },
            },
            Binding {
                group: 0,
                binding: 1,
                resource: BindingResource::ImageView(table),
            },
        ],
    )?;
    let mut projection = [[0u32; 4]; 12];
    for (row, axis) in projection[..2].iter_mut().zip(requests.axes) {
        row[..3].copy_from_slice(&axis.map(|v| (v as f32).to_bits()));
    }
    projection[2] = requests.depth_plane.map(f32::to_bits);
    projection[3] = requests.bias.map(f32::to_bits);
    for (row, origin) in projection[4..].iter_mut().zip(requests.origins) {
        row[..2].copy_from_slice(&origin.map(|v| v as u32));
    }
    Ok(VirtualSun {
        atlas: graph.image_view(atlas, 0, 0)?,
        table,
        projection,
        distance: requests.distance,
    })
}
