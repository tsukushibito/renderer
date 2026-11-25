//! File framing and shader stage checks for runtime Program imports.
//!
//! The trusted descriptor contract still covers bytecode semantics, ABI and
//! access guarantees.

#[derive(Clone, Copy)]
pub(super) enum Stage {
    Vertex,
    Fragment,
    Compute,
}

pub(super) fn valid_spirv(bytes: &[u8], stage: Stage) -> bool {
    // https://registry.khronos.org/SPIR-V/specs/unified1/SPIRV.html
    const MAGIC: u32 = 0x0723_0203;
    const HEADER_BYTES: usize = 20;
    const OP_ENTRY_POINT: u16 = 15;
    let Some(mut instructions) = bytes.get(HEADER_BYTES..) else {
        return false;
    };
    let id_bound = word(bytes, 12);
    let reserved = word(bytes, 16);
    if !bytes.len().is_multiple_of(4)
        || word(bytes, 0) != Some(MAGIC)
        || !matches!(
            word(bytes, 4),
            Some(0x0001_0000 | 0x0001_0100 | 0x0001_0200 | 0x0001_0300)
        )
        || id_bound == Some(0)
        || reserved != Some(0)
    {
        return false;
    }
    let execution_model = match stage {
        Stage::Vertex => 0,
        Stage::Fragment => 4,
        Stage::Compute => 5,
    };
    let mut found_entry = false;
    while let Some(header) = instructions.first_chunk::<4>() {
        let word_count = usize::from(u16::from_le_bytes([header[2], header[3]]));
        let Some((instruction, rest)) = instructions.split_at_checked(word_count * 4) else {
            return false;
        };
        if word_count == 0 {
            return false;
        }
        let opcode = u16::from_le_bytes([header[0], header[1]]);
        if opcode == OP_ENTRY_POINT
            && word(instruction, 4) == Some(execution_model)
            // The literal string follows the execution model and function ID.
            && instruction.get(12..20) == Some(b"main\0\0\0\0")
        {
            found_entry = true;
        }
        instructions = rest;
    }
    found_entry
}

pub(super) fn valid_dxil(bytes: &[u8], stage: Stage) -> bool {
    let Some(part) = dxil_part(bytes) else {
        return false;
    };
    let shader_kind = match stage {
        Stage::Vertex => 1,
        Stage::Fragment => 0,
        Stage::Compute => 5,
    };
    // ProgramVersion encodes shader kind in the high 16 bits and model 6.0
    // in the low bits. The bitcode header begins eight bytes into the part.
    word(part, 0) == Some((shader_kind << 16) | 0x60) && dxil_bitcode(part).is_some()
}

// https://llvm.org/docs/DirectX/DXContainer.html
// https://github.com/microsoft/DirectXShaderCompiler/blob/main/lib/DxilContainer/DxilContainer.cpp
fn dxil_part(bytes: &[u8]) -> Option<&[u8]> {
    const HEADER_BYTES: usize = 32;
    if bytes.get(..4)? != b"DXBC" || word(bytes, 20)? != 1 || size(bytes, 24)? != bytes.len() {
        return None;
    }
    let table_bytes = size(bytes, 28)?.checked_mul(4)?;
    let (offsets, mut parts) = bytes.get(HEADER_BYTES..)?.split_at_checked(table_bytes)?;
    let mut offset = HEADER_BYTES + table_bytes;
    let mut result = None;
    for entry in offsets.as_chunks::<4>().0 {
        if size(entry, 0)? != offset {
            return None;
        }
        let (header, data) = parts.split_first_chunk::<8>()?;
        let (part, rest) = data.split_at_checked(size(header, 4)?)?;
        if header.starts_with(b"DXIL") {
            if result.is_some() {
                return None;
            }
            result = Some(part);
        }
        parts = rest;
        offset = bytes.len() - parts.len();
    }
    if !parts.is_empty() {
        return None;
    }
    result
}

fn dxil_bitcode(part: &[u8]) -> Option<&[u8]> {
    const BITCODE_HEADER_BYTES: usize = 16;
    let program = part.get(..size(part, 4)?.checked_mul(4)?)?;
    let bitcode_header = program.get(8..)?;
    let bitcode_offset = size(bitcode_header, 8)?;
    if bitcode_header.get(..4)? != b"DXIL" || bitcode_offset < BITCODE_HEADER_BYTES {
        return None;
    }
    let bitcode = bitcode_header
        .get(bitcode_offset..)?
        .get(..size(bitcode_header, 12)?)?;
    bitcode.starts_with(b"BC\xC0\xDE").then_some(bitcode)
}

fn word(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(offset..)?.first_chunk()?))
}

fn size(bytes: &[u8], offset: usize) -> Option<usize> {
    usize::try_from(word(bytes, offset)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERTEX_SPIRV: &[u8] = include_bytes!("../../shaders/mesh.vert.spv");
    const VERTEX_DXIL: &[u8] = include_bytes!("../../shaders/dxil/mesh.vert.dxil");

    #[test]
    fn accepts_reviewed_binaries_only_in_the_matching_stage() {
        for (stage, wrong_stage, spirv, dxil) in [
            (Stage::Vertex, Stage::Fragment, VERTEX_SPIRV, VERTEX_DXIL),
            (
                Stage::Fragment,
                Stage::Compute,
                include_bytes!("../../shaders/mesh.frag.spv").as_slice(),
                include_bytes!("../../shaders/dxil/mesh.frag.dxil").as_slice(),
            ),
            (
                Stage::Compute,
                Stage::Vertex,
                include_bytes!("../../shaders/uniform.comp.spv").as_slice(),
                include_bytes!("../../shaders/dxil/uniform.comp.dxil").as_slice(),
            ),
        ] {
            assert!(valid_spirv(spirv, stage));
            assert!(valid_dxil(dxil, stage));
            assert!(!valid_spirv(dxil, stage));
            assert!(!valid_dxil(spirv, stage));
            assert!(!valid_spirv(spirv, wrong_stage));
            assert!(!valid_dxil(dxil, wrong_stage));
        }
    }

    #[test]
    fn rejects_malformed_spirv_instruction_lengths() {
        assert!(!valid_spirv(
            &VERTEX_SPIRV[..VERTEX_SPIRV.len() - 1],
            Stage::Vertex
        ));
        // Malformed instructions after the entry point must still fail.
        for invalid_word in [0_u32, 0xFFFF_0000] {
            let mut malformed = VERTEX_SPIRV.to_vec();
            malformed.extend_from_slice(&invalid_word.to_le_bytes());
            assert!(!valid_spirv(&malformed, Stage::Vertex));
        }
    }

    #[test]
    fn rejects_spirv_with_a_different_entry_point_name() {
        let mut renamed = VERTEX_SPIRV.to_vec();
        // Both the debug name and the entry point can contain this string.
        while let Some(offset) = renamed.windows(5).position(|bytes| bytes == b"main\0") {
            renamed[offset] = b'M';
        }
        assert!(!valid_spirv(&renamed, Stage::Vertex));
    }

    #[test]
    fn rejects_out_of_bounds_dxil_container_offsets() {
        assert!(!valid_dxil(
            &VERTEX_DXIL[..VERTEX_DXIL.len() - 1],
            Stage::Vertex
        ));
        let mut malformed = VERTEX_DXIL.to_vec();
        malformed[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!valid_dxil(&malformed, Stage::Vertex));

        let mut malformed = VERTEX_DXIL.to_vec();
        let part_start =
            dxil_part(VERTEX_DXIL).unwrap().as_ptr() as usize - VERTEX_DXIL.as_ptr() as usize;
        let bitcode_offset_field = part_start + 16;
        malformed[bitcode_offset_field..][..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!valid_dxil(&malformed, Stage::Vertex));
    }
}
