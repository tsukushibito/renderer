//! Immutable mesh bounds replace CPU byte retention. Arbitrary Graph input
//! retains byte provenance because partial writes/copies must establish a sound
//! index guarantee. Unknown holes never become initialized index contents.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(super) struct BufferProvenance {
    segments: BTreeMap<u64, Segment>,
    // A content maximum is independent of a draw's vertex/Device bounds. Keep
    // one prefix summary; changing only those bounds never rescans the bytes.
    validation: Option<IndexValidation>,
    // Managed meshes need no CPU copy. This private proof is installed only by
    // uploads carrying MeshData's immutable bound, at the same byte positions.
    mesh: Option<MeshIndexSummary>,
    #[cfg(test)]
    validation_scans: usize,
}

#[derive(Clone, Copy)]
struct IndexValidation {
    count: u32,
    maximum: Result<u32, IndexError>,
}

#[derive(Clone, Copy)]
struct MeshIndexSummary {
    byte_len: u64,
    known_prefix: u64,
    maximum: u32,
}

#[derive(Clone)]
struct Segment {
    start: u64,
    bytes: Arc<[u8]>,
    range: Range<usize>,
}

impl Segment {
    fn end(&self) -> u64 {
        self.start + self.range.len() as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IndexError {
    UnknownContents,
    VertexBounds { index: u32, vertex_count: u64 },
    DeviceLimit { index: u32, maximum: u32 },
}

impl BufferProvenance {
    #[cfg(test)]
    pub(super) fn test_evidence(&self) -> (usize, usize) {
        (self.segments.len(), self.validation_scans)
    }

    /// Mesh uploads are generated in stream order, at four-byte boundaries.
    /// Only a complete stream can become the immutable mesh's index proof.
    /// Sparse/out-of-order writes remain unproven, never initialized by fiat.
    pub(super) fn write_mesh_indices(&mut self, target: Range<u64>, byte_len: u64, maximum: u32) {
        self.segments.clear();
        self.validation = None;
        let previous = self.mesh.take().filter(|proof| proof.byte_len == byte_len);
        let mut proof = previous.unwrap_or(MeshIndexSummary {
            byte_len,
            known_prefix: 0,
            maximum,
        });
        proof.maximum = proof.maximum.max(maximum);
        if target.start <= proof.known_prefix && target.end <= byte_len {
            proof.known_prefix = proof.known_prefix.max(target.end);
        }
        self.mesh = Some(proof);
    }

    /// `target` and `bytes` describe an already validated graph upload.
    #[cfg(test)]
    pub(super) fn write(&mut self, target: Range<u64>, bytes: Arc<[u8]>) {
        let length = bytes.len();
        self.write_slice(target, bytes, 0..length);
    }

    /// The source slice is validated by Graph, including its destination length.
    pub(super) fn write_slice(
        &mut self,
        target: Range<u64>,
        bytes: Arc<[u8]>,
        source: Range<usize>,
    ) {
        debug_assert!(bytes.get(source.clone()).is_some());
        debug_assert_eq!(target.end - target.start, source.len() as u64);
        self.replace(
            target.clone(),
            Self {
                segments: BTreeMap::from([(
                    target.start,
                    Segment {
                        start: target.start,
                        bytes,
                        range: source,
                    },
                )]),
                ..Self::default()
            },
        );
    }

    /// Return shared known slices at their destination positions. The source and
    /// destination ranges are graph-validated; gaps are deliberately preserved.
    /// Obtain this before replacing the destination to support disjoint self-copy.
    pub(super) fn copied(&self, source: Range<u64>, destination: u64) -> Self {
        // An exact complete mesh stream keeps its proof. Arbitrary byte slices
        // cannot reconstruct an exact index maximum without the source bytes.
        let mesh = self.mesh.filter(|proof| {
            destination == 0
                && source == (0..proof.byte_len)
                && proof.known_prefix == proof.byte_len
        });
        Self {
            mesh,
            segments: self
                .overlapping(&source)
                .filter_map(|segment| {
                    let start = source.start.max(segment.start);
                    let end = source.end.min(segment.end());
                    (start < end).then(|| {
                        let position = destination + (start - source.start);
                        (
                            position,
                            Segment {
                                start: position,
                                bytes: Arc::clone(&segment.bytes),
                                range: segment.range.start + (start - segment.start) as usize
                                    ..segment.range.start + (end - segment.start) as usize,
                            },
                        )
                    })
                })
                .collect(),
            ..Self::default()
        }
    }

    /// Overwrite the whole target, including unknown portions of a GPU copy.
    pub(super) fn replace(&mut self, target: Range<u64>, mut contents: Self) {
        // Mutation invalidates both successful and failed validation results.
        // Other clones still describe their old immutable contents.
        self.validation = None;
        // A generic mutation invalidates a managed proof. Only a complete,
        // position-preserving copy can install another such proof.
        self.mesh = contents
            .mesh
            .take()
            .filter(|proof| target == (0..proof.byte_len));
        let keys = self
            .overlapping(&target)
            .map(|segment| segment.start)
            .collect::<Vec<_>>();
        for key in keys {
            let segment = self.segments.remove(&key).expect("overlapping segment");
            if segment.start < target.start {
                let count = (target.start - segment.start) as usize;
                self.segments.insert(
                    segment.start,
                    Segment {
                        start: segment.start,
                        bytes: Arc::clone(&segment.bytes),
                        range: segment.range.start..segment.range.start + count,
                    },
                );
            }
            if segment.end() > target.end {
                let skip = (target.end - segment.start) as usize;
                self.segments.insert(
                    target.end,
                    Segment {
                        start: target.end,
                        bytes: segment.bytes,
                        range: segment.range.start + skip..segment.range.end,
                    },
                );
            }
        }
        self.segments.extend(contents.segments);
    }

    /// Include the predecessor only if it overlaps, then visit intersecting
    /// segments. Updating a narrow range never scans unrelated known contents.
    fn overlapping(&self, range: &Range<u64>) -> impl Iterator<Item = &Segment> {
        let first = self
            .segments
            .range(..=range.start)
            .next_back()
            .map_or(range.start, |(&start, _)| start);
        self.segments
            .range(first..range.end)
            .map(|(_, segment)| segment)
            .filter(move |segment| segment.end() > range.start)
    }

    /// Validate little-endian u32 indices without reconstructing a byte buffer.
    /// Partial writes may split an index across multiple backing slices.
    pub(super) fn validate_indices(
        &mut self,
        count: u32,
        vertex_count: u64,
        maximum: u32,
    ) -> Result<(), IndexError> {
        if count == 0 {
            return Ok(());
        }
        if let Some(proof) = self.mesh {
            if u64::from(count) * 4 != proof.byte_len || proof.known_prefix != proof.byte_len {
                return Err(IndexError::UnknownContents);
            }
            return validate_index(proof.maximum, vertex_count, maximum);
        }
        let content_maximum = match self.validation {
            Some(validation) if validation.count == count => validation.maximum,
            _ => {
                #[cfg(test)]
                {
                    self.validation_scans += 1;
                }
                let maximum = self.scan_indices(count);
                self.validation = Some(IndexValidation { count, maximum });
                maximum
            }
        }?;
        validate_index(content_maximum, vertex_count, maximum)
    }

    fn scan_indices(&self, count: u32) -> Result<u32, IndexError> {
        let end = u64::from(count) * 4;
        let mut content_maximum = 0;
        let mut cursor = 0;
        let mut pending = [0u8; 4];
        let mut filled = 0;
        for segment in self.segments.values() {
            if segment.end() <= cursor {
                continue;
            }
            if segment.start > cursor {
                return Err(IndexError::UnknownContents);
            }
            let begin = segment.range.start + (cursor - segment.start) as usize;
            let next = end.min(segment.end());
            let length = (next - cursor) as usize;
            let mut bytes = &segment.bytes[begin..begin + length];
            if filled != 0 {
                let length = (4 - filled).min(bytes.len());
                pending[filled..filled + length].copy_from_slice(&bytes[..length]);
                filled += length;
                bytes = &bytes[length..];
                if filled == 4 {
                    content_maximum = content_maximum.max(u32::from_le_bytes(pending));
                    filled = 0;
                }
            }
            let (indices, tail) = bytes.as_chunks::<4>();
            for index in indices {
                content_maximum = content_maximum.max(u32::from_le_bytes(*index));
            }
            if !tail.is_empty() {
                pending[..tail.len()].copy_from_slice(tail);
                filled = tail.len();
            }
            cursor = next;
            if cursor == end {
                debug_assert_eq!(filled, 0);
                return Ok(content_maximum);
            }
        }
        if cursor == end {
            Ok(content_maximum)
        } else {
            Err(IndexError::UnknownContents)
        }
    }
}

fn validate_index(index: u32, vertex_count: u64, maximum: u32) -> Result<(), IndexError> {
    if u64::from(index) >= vertex_count {
        return Err(IndexError::VertexBounds {
            index,
            vertex_count,
        });
    }
    if index > maximum {
        return Err(IndexError::DeviceLimit { index, maximum });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(contents: &BufferProvenance, range: Range<u64>) -> Vec<Option<u8>> {
        range
            .map(|offset| {
                contents.segments.values().find_map(|segment| {
                    (segment.start <= offset && offset < segment.end()).then(|| {
                        segment.bytes[segment.range.start + (offset - segment.start) as usize]
                    })
                })
            })
            .collect()
    }

    #[test]
    fn source_slices_keep_backing_offsets_across_partial_indices_and_copies() {
        let mut input = vec![0xcc; 3];
        for index in [3u32, 7, 1, 9] {
            input.extend_from_slice(&index.to_le_bytes());
        }
        let input: Arc<[u8]> = input.into();
        let mut contents = BufferProvenance::default();
        for (target, source) in [(0..2, 3..5), (2..6, 5..9), (6..12, 9..15)] {
            contents.write_slice(target, Arc::clone(&input), source);
        }
        assert!(
            contents
                .segments
                .values()
                .all(|segment| Arc::ptr_eq(&segment.bytes, &input))
        );
        assert_eq!(contents.validate_indices(3, 8, u32::MAX), Ok(()));
        let copied = contents.copied(2..10, 4);
        assert_eq!(bytes(&copied, 4..12), bytes(&contents, 2..10));
        assert!(
            copied
                .segments
                .values()
                .all(|segment| Arc::ptr_eq(&segment.bytes, &input))
        );
        // A patch from another slice of the same allocation invalidates the
        // previous successful bounds check and uses that slice's exact bytes.
        contents.write_slice(4..8, Arc::clone(&input), 15..19);
        assert_eq!(
            contents.validate_indices(3, 8, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 9,
                vertex_count: 8
            })
        );
        contents.replace(1..2, BufferProvenance::default());
        assert_eq!(
            contents.validate_indices(3, 10, u32::MAX),
            Err(IndexError::UnknownContents)
        );
    }

    #[test]
    fn copies_share_input_and_preserve_partial_known_contents_and_holes() {
        let upload: Arc<[u8]> = Arc::from([1u8, 2, 3, 4, 5, 6, 7, 8]);
        let mut source = BufferProvenance::default();
        source.write(0..8, Arc::clone(&upload));
        source.write(2..4, Arc::from([9u8, 10]));
        let mut target = BufferProvenance::default();
        target.write(0..16, Arc::from([99u8; 16]));
        source.replace(3..5, BufferProvenance::default());
        target.replace(8..14, source.copied(1..7, 8));
        assert_eq!(
            bytes(&target, 8..14),
            [Some(2), Some(9), None, None, Some(6), Some(7)]
        );
        assert_eq!(bytes(&target, 7..8), [Some(99)]);
        assert_eq!(bytes(&target, 14..16), [Some(99), Some(99)]);
        assert!(
            target
                .segments
                .values()
                .any(|segment| Arc::ptr_eq(&segment.bytes, &upload))
        );
        // A disjoint same-buffer copy snapshots its slices before overwriting.
        let copied = source.copied(0..8, 8);
        source.replace(8..16, copied);
        assert_eq!(bytes(&source, 0..8), bytes(&source, 8..16));
    }

    #[test]
    fn index_checks_span_shared_slices_and_enforce_both_bounds() {
        let maximum: u32 = (1 << 24) - 1;
        let mut contents = BufferProvenance::default();
        contents.write(0..12, Arc::from([0u8; 12]));
        let bytes = maximum.to_le_bytes();
        contents.write(1..2, Arc::from([bytes[1]]));
        contents.write(0..1, Arc::from([bytes[0]]));
        contents.write(2..4, Arc::from([bytes[2], bytes[3]]));
        assert_eq!(
            contents.validate_indices(3, u64::from(maximum) + 1, maximum),
            Ok(())
        );
        assert_eq!(
            contents.validate_indices(3, u64::from(maximum), maximum),
            Err(IndexError::VertexBounds {
                index: maximum,
                vertex_count: u64::from(maximum)
            })
        );
        assert_eq!(
            contents.validate_indices(3, u64::MAX, maximum - 1),
            Err(IndexError::DeviceLimit {
                index: maximum,
                maximum: maximum - 1
            })
        );
        contents.replace(5..6, BufferProvenance::default());
        assert_eq!(
            contents.validate_indices(3, u64::MAX, u32::MAX),
            Err(IndexError::UnknownContents)
        );
        let full = Arc::from(u32::MAX.to_le_bytes());
        contents.write(0..4, full);
        assert_eq!(
            contents.validate_indices(1, u64::from(u32::MAX) + 1, u32::MAX),
            Ok(())
        );
        contents.write(0..4, Arc::from([0u8; 4]));
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
    }

    #[test]
    fn partial_updates_and_copies_match_known_byte_model() {
        let mut contents = BufferProvenance::default();
        let mut expected = vec![None; 128];
        let mut random = 0x7265_6e64_6572u64;
        for step in 0..1_200 {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let start = (random as usize >> 8) % 112;
            let length = 1 + (random as usize >> 24) % 16;
            let range = start as u64..(start + length) as u64;
            match step % 3 {
                0 => {
                    let data: Arc<[u8]> = (0..length)
                        .map(|i| (step + i) as u8)
                        .collect::<Vec<_>>()
                        .into();
                    expected[start..start + length]
                        .iter_mut()
                        .zip(data.iter())
                        .for_each(|(target, &value)| *target = Some(value));
                    contents.write(range, data);
                }
                1 => {
                    expected[start..start + length].fill(None);
                    contents.replace(range, BufferProvenance::default());
                }
                _ => {
                    // Snapshot before replacement, like a disjoint same-buffer
                    // GPU copy. Both initialized bytes and holes are copied.
                    let source = start % 48;
                    let target = 64 + source;
                    let values = expected[source..source + length].to_vec();
                    let copied =
                        contents.copied(source as u64..(source + length) as u64, target as u64);
                    contents.replace(target as u64..(target + length) as u64, copied);
                    expected[target..target + length].copy_from_slice(&values);
                }
            }
            assert_eq!(bytes(&contents, 0..128), expected, "update {step}");
        }
    }

    #[test]
    fn validation_reuses_content_maximum_across_vertex_and_device_bounds() {
        let mut contents = BufferProvenance::default();
        contents.write(0..8, Arc::from([0u8; 8]));
        for _ in 0..100 {
            assert_eq!(contents.validate_indices(2, 1, 0), Ok(()));
        }
        assert_eq!(contents.validation_scans, 1);
        let mut old_contents = contents.clone();
        // Vertex and Device bounds are applied to the content maximum, so
        // changing them does not scan again. A different prefix is summarized
        // independently, including when the previous draw's bounds failed.
        assert!(matches!(
            contents.validate_indices(2, 0, 0),
            Err(IndexError::VertexBounds { .. })
        ));
        assert_eq!(contents.validation_scans, 1);
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
        assert_eq!(contents.validation_scans, 2);
        assert_eq!(contents.validate_indices(1, 1, 1), Ok(()));
        assert_eq!(contents.validation_scans, 2);

        // A partial upload invalidates the cache; clones keep their own old
        // contents and valid cache because all backing byte slices are immutable.
        contents.write(0..1, Arc::from([2u8]));
        assert_eq!(
            contents.validate_indices(1, 1, 1),
            Err(IndexError::VertexBounds {
                index: 2,
                vertex_count: 1
            })
        );
        assert_eq!(old_contents.validate_indices(2, 1, 0), Ok(()));
        assert_eq!(old_contents.validation_scans, 1);
        assert_eq!(
            contents.validate_indices(1, 3, 1),
            Err(IndexError::DeviceLimit {
                index: 2,
                maximum: 1
            })
        );
        // Copy replacement and unknown holes also invalidate prior failures.
        contents.replace(0..4, old_contents.copied(0..4, 0));
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
        contents.replace(1..2, BufferProvenance::default());
        assert_eq!(
            contents.validate_indices(1, 1, 0),
            Err(IndexError::UnknownContents)
        );
        contents.write(1..2, Arc::from([0u8]));
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
    }

    #[test]
    fn managed_mesh_has_no_payload_or_scans_and_rechecks_dynamic_bounds() {
        let mut contents = BufferProvenance::default();
        contents.write_mesh_indices(0..4, 12, 2);
        assert_eq!(
            contents.validate_indices(3, 3, 2),
            Err(IndexError::UnknownContents)
        );
        let mut unaccepted = contents.clone();
        contents.write_mesh_indices(4..12, 12, 2);
        for draw in 0..32 {
            assert_eq!(contents.validate_indices(3, 3 + draw % 2, 2), Ok(()));
        }
        assert_eq!(contents.test_evidence(), (0, 0));
        assert_eq!(
            unaccepted.validate_indices(3, 3, 2),
            Err(IndexError::UnknownContents),
            "preparing another snapshot must not publish its accepted contents"
        );
        assert!(matches!(
            contents.validate_indices(3, 2, 2),
            Err(IndexError::VertexBounds { .. })
        ));
        assert!(matches!(
            contents.validate_indices(3, 3, 1),
            Err(IndexError::DeviceLimit { .. })
        ));
        assert_eq!(
            contents.validate_indices(1, 3, 2),
            Err(IndexError::UnknownContents),
            "a whole-mesh maximum is not an exact prefix maximum"
        );
        assert_eq!(contents.test_evidence(), (0, 0));
        // Replacing a stream with another mesh of the same byte length must
        // never lower the bound while old chunks can still contribute bytes.
        let mut mixed = BufferProvenance::default();
        mixed.write_mesh_indices(0..4, 12, 7);
        mixed.write_mesh_indices(4..12, 12, 2);
        assert_eq!(mixed.validate_indices(3, 8, 7), Ok(()));
        assert_eq!(
            mixed.validate_indices(3, 3, 7),
            Err(IndexError::VertexBounds {
                index: 7,
                vertex_count: 3
            })
        );
    }

    #[test]
    fn managed_mesh_copy_and_mutation_have_explicit_proof_boundaries() {
        let mut original = BufferProvenance::default();
        original.write_mesh_indices(0..12, 12, 2);
        let mut copied = BufferProvenance::default();
        copied.replace(0..12, original.copied(0..12, 0));
        assert_eq!(copied.validate_indices(3, 3, 2), Ok(()));
        assert_eq!(copied.test_evidence(), (0, 0));
        let mut partial = original.copied(0..4, 0);
        assert_eq!(
            partial.validate_indices(1, 3, 2),
            Err(IndexError::UnknownContents)
        );
        copied.write(4..8, Arc::from(0u32.to_le_bytes()));
        assert_eq!(
            copied.validate_indices(3, 3, 2),
            Err(IndexError::UnknownContents),
            "arbitrary writes cannot reuse the unmodified mesh proof"
        );
        assert_eq!(
            original.validate_indices(3, 3, 2),
            Ok(()),
            "immutable old snapshots keep their own proof"
        );
        copied.write(0..12, Arc::from([0u8; 12]));
        assert_eq!(
            copied.validate_indices(3, 1, 0),
            Ok(()),
            "a complete generic replacement re-establishes byte provenance"
        );
        let mut sparse = BufferProvenance::default();
        sparse.write_mesh_indices(4..12, 12, 2);
        sparse.write_mesh_indices(0..4, 12, 2);
        assert_eq!(
            sparse.validate_indices(3, 3, 2),
            Err(IndexError::UnknownContents)
        );
    }

    #[test]
    fn generic_alternating_vertex_bounds_share_one_content_summary() {
        let mut contents = BufferProvenance::default();
        contents.write(
            0..12,
            [0u32, 1, 2]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
                .into(),
        );
        for draw in 0..32 {
            assert_eq!(contents.validate_indices(3, 3 + draw % 2, u32::MAX), Ok(()));
        }
        assert_eq!(contents.validation_scans, 1);
    }
}
