//! Immutable mesh bounds replace CPU byte retention. Arbitrary Graph input
//! retains byte provenance because partial writes/copies must establish a sound
//! index guarantee. Unknown holes never become initialized index contents.

use std::ops::Range;
#[cfg(test)]
use std::sync::Arc;

use crate::graph::CompiledGraph;

/// Sorted disjoint known byte slices, with storage prepared at a change boundary.
/// Ordinary updates never grow or release this metadata allocation. Graph upload
/// pins borrow the pool's prepared backing; the runtime must keep the GraphPool
/// alive until its canonical provenance and all work snapshots have been cleared.
#[derive(Default)]
pub(crate) struct BufferProvenance {
    segments: Vec<Segment>,
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
enum SourceBytes {
    Graph {
        graph: CompiledGraph,
        pass: usize,
    },
    // CPU tests keep a separate owner alive when measuring ordinary mutations.
    // Production uploads pin Graph storage, including asset-backed operations.
    #[cfg(test)]
    Asset(Arc<[u8]>),
}

impl SourceBytes {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Graph { graph, pass } => graph.upload_data(&graph.passes[*pass].operation),
            #[cfg(test)]
            Self::Asset(bytes) => bytes,
        }
    }
}

#[derive(Clone)]
struct Segment {
    start: u64,
    bytes: SourceBytes,
    range: Range<usize>,
}

impl Segment {
    fn end(&self) -> u64 {
        self.start + self.range.len() as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProvenanceCapacityError {
    pub(crate) required: usize,
    pub(crate) capacity: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexError {
    UnknownContents,
    VertexBounds { index: u32, vertex_count: u64 },
    DeviceLimit { index: u32, maximum: u32 },
}

impl BufferProvenance {
    pub(crate) fn with_capacity(segments: usize) -> Self {
        Self {
            segments: Vec::with_capacity(segments),
            ..Self::default()
        }
    }

    /// Explicit preparation/change boundary; never called by ordinary mutation.
    #[cfg(test)]
    fn reserve_segments(&mut self, capacity: usize) {
        if self.segments.capacity() < capacity {
            self.segments.reserve_exact(capacity - self.segments.len());
        }
    }

    pub(crate) fn segment_capacity(&self) -> usize {
        self.segments.capacity()
    }

    fn require_capacity(&self, required: usize) -> Result<(), ProvenanceCapacityError> {
        if required > self.segment_capacity() {
            return Err(ProvenanceCapacityError {
                required,
                capacity: self.segment_capacity(),
            });
        }
        Ok(())
    }

    /// The external GraphPool owner keeps its allocations alive when pins drop.
    /// Clearing state does not discard the prepared segment storage.
    pub(crate) fn clear(&mut self) {
        self.segments.clear();
        self.validation = None;
        self.mesh = None;
        #[cfg(test)]
        {
            self.validation_scans = 0;
        }
    }

    /// Copy an independent metadata version into already prepared storage. The
    /// immutable backing and prefix summary can be shared without revalidation.
    #[cfg(any(feature = "vulkan", test))]
    pub(crate) fn copy_from(&mut self, source: &Self) -> Result<(), ProvenanceCapacityError> {
        self.require_capacity(source.segments.len())?;
        self.segments.clear();
        self.segments.extend(source.segments.iter().cloned());
        self.validation = source.validation;
        self.mesh = source.mesh;
        #[cfg(test)]
        {
            self.validation_scans = source.validation_scans;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn test_evidence(&self) -> (usize, usize) {
        (self.segments.len(), self.validation_scans)
    }

    /// Mesh uploads are generated in stream order, at four-byte boundaries.
    /// Only a complete stream can become the immutable mesh's index proof.
    /// Sparse/out-of-order writes remain unproven, never initialized by fiat.
    pub(crate) fn write_mesh_indices(&mut self, target: Range<u64>, byte_len: u64, maximum: u32) {
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

    #[cfg(test)]
    fn write(&mut self, target: Range<u64>, bytes: Arc<[u8]>) {
        let length = bytes.len();
        self.write_slice(target, &bytes, 0..length)
            .expect("prepared test provenance capacity");
    }

    /// The source range indexes `upload_data`, which already applies the Graph
    /// operation's original source range. The immutable Graph guarantees the
    /// target and source slice. A pin prevents that upload's prepared backing
    /// from being reused while retained.
    pub(crate) fn write_graph_slice(
        &mut self,
        target: Range<u64>,
        graph: &CompiledGraph,
        pass: usize,
        source: Range<usize>,
    ) -> Result<(), ProvenanceCapacityError> {
        let bytes = graph.upload_data(&graph.passes[pass].operation);
        debug_assert!(bytes.get(source.clone()).is_some());
        debug_assert_eq!(target.end - target.start, source.len() as u64);
        self.write_into(target, source, || SourceBytes::Graph {
            graph: graph.clone(),
            pass,
        })
    }

    #[cfg(test)]
    pub(crate) fn write_slice(
        &mut self,
        target: Range<u64>,
        bytes: &Arc<[u8]>,
        source: Range<usize>,
    ) -> Result<(), ProvenanceCapacityError> {
        debug_assert!(bytes.get(source.clone()).is_some());
        debug_assert_eq!(target.end - target.start, source.len() as u64);
        self.write_into(target, source, || SourceBytes::Asset(Arc::clone(bytes)))
    }

    fn write_into(
        &mut self,
        target: Range<u64>,
        source: Range<usize>,
        bytes: impl FnOnce() -> SourceBytes,
    ) -> Result<(), ProvenanceCapacityError> {
        if target.is_empty() {
            return Ok(());
        }
        let overlap = self.overlap_indices(&target);
        self.require_capacity(self.replacement_len(&target, &overlap, 1))?;
        // Clone only after the capacity check, preserving ownership on failure.
        let segment = Segment {
            start: target.start,
            bytes: bytes(),
            range: source,
        };
        self.replace_segments(target, overlap, std::slice::from_ref(&segment), None);
        Ok(())
    }

    /// Copy known slices into prepared scratch at their destination positions,
    /// preserving holes. Snapshot first, then replace, for disjoint self-copies.
    pub(crate) fn copy_into(
        &self,
        source: Range<u64>,
        destination: u64,
        output: &mut Self,
    ) -> Result<(), ProvenanceCapacityError> {
        let overlap = self.overlap_indices(&source);
        output.require_capacity(overlap.len())?;
        output.clear();
        output.mesh = self.mesh.filter(|proof| {
            destination == 0
                && source == (0..proof.byte_len)
                && proof.known_prefix == proof.byte_len
        });
        for segment in &self.segments[overlap] {
            let start = source.start.max(segment.start);
            let end = source.end.min(segment.end());
            let position = destination + (start - source.start);
            output.segments.push(Segment {
                start: position,
                bytes: segment.bytes.clone(),
                range: segment.range.start + (start - segment.start) as usize
                    ..segment.range.start + (end - segment.start) as usize,
            });
        }
        Ok(())
    }

    /// Overwrite the whole target, including unknown portions of a GPU copy.
    /// Incoming known segments have already been positioned within `target` by
    /// copy_into. Capacity failure leaves data, proof and cached summary intact.
    pub(crate) fn replace(
        &mut self,
        target: Range<u64>,
        contents: &Self,
    ) -> Result<(), ProvenanceCapacityError> {
        if target.is_empty() {
            return Ok(());
        }
        let overlap = self.overlap_indices(&target);
        self.require_capacity(self.replacement_len(&target, &overlap, contents.segments.len()))?;
        self.replace_segments(target, overlap, &contents.segments, contents.mesh);
        Ok(())
    }

    pub(crate) fn clear_range(
        &mut self,
        target: Range<u64>,
    ) -> Result<(), ProvenanceCapacityError> {
        if target.is_empty() {
            return Ok(());
        }
        let overlap = self.overlap_indices(&target);
        self.require_capacity(self.replacement_len(&target, &overlap, 0))?;
        self.replace_segments(target, overlap, &[], None);
        Ok(())
    }

    fn replacement_len(
        &self,
        target: &Range<u64>,
        overlap: &Range<usize>,
        incoming: usize,
    ) -> usize {
        let retained = self.segments[overlap.clone()]
            .first()
            .is_some_and(|segment| segment.start < target.start) as usize
            + self.segments[overlap.clone()]
                .last()
                .is_some_and(|segment| segment.end() > target.end) as usize;
        self.segments.len() - overlap.len() + retained + incoming
    }

    fn replace_segments(
        &mut self,
        target: Range<u64>,
        overlap: Range<usize>,
        incoming: &[Segment],
        mesh: Option<MeshIndexSummary>,
    ) {
        let left = self.segments[overlap.clone()].first().and_then(|segment| {
            (segment.start < target.start).then(|| Segment {
                start: segment.start,
                bytes: segment.bytes.clone(),
                range: segment.range.start
                    ..segment.range.start + (target.start - segment.start) as usize,
            })
        });
        let right = self.segments[overlap.clone()].last().and_then(|segment| {
            (segment.end() > target.end).then(|| Segment {
                start: target.end,
                bytes: segment.bytes.clone(),
                range: segment.range.start + (target.end - segment.start) as usize
                    ..segment.range.end,
            })
        });
        let mut position = overlap.start;
        self.segments.drain(overlap);
        if let Some(segment) = left {
            self.segments.insert(position, segment);
            position += 1;
        }
        for segment in incoming {
            self.segments.insert(position, segment.clone());
            position += 1;
        }
        if let Some(segment) = right {
            self.segments.insert(position, segment);
        }
        // Every successful generic mutation invalidates successful and failed
        // validation. Other prepared snapshots retain their immutable version.
        self.validation = None;
        self.mesh = mesh.filter(|proof| target == (0..proof.byte_len));
    }

    /// Include only intersecting segments. A narrow write/copy locates its
    /// bounds by binary search without scanning unrelated known contents.
    fn overlap_indices(&self, range: &Range<u64>) -> Range<usize> {
        let start = self
            .segments
            .partition_point(|segment| segment.end() <= range.start);
        if range.is_empty() {
            return start..start;
        }
        let count = self.segments[start..].partition_point(|segment| segment.start < range.end);
        start..start + count
    }

    /// Validate little-endian u32 indices without reconstructing a byte buffer.
    /// Partial writes may split an index across multiple backing slices.
    pub(crate) fn validate_indices(
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
        for segment in &self.segments {
            if segment.end() <= cursor {
                continue;
            }
            if segment.start > cursor {
                return Err(IndexError::UnknownContents);
            }
            let begin = segment.range.start + (cursor - segment.start) as usize;
            let next = end.min(segment.end());
            let length = (next - cursor) as usize;
            let mut bytes = &segment.bytes.bytes()[begin..begin + length];
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

    fn prepared() -> BufferProvenance {
        BufferProvenance::with_capacity(256)
    }

    fn snapshot(source: &BufferProvenance) -> BufferProvenance {
        let mut result = prepared();
        result.copy_from(source).unwrap();
        result
    }

    fn copy_range(
        source: &BufferProvenance,
        range: Range<u64>,
        destination: u64,
    ) -> BufferProvenance {
        let mut result = prepared();
        source.copy_into(range, destination, &mut result).unwrap();
        result
    }

    fn shares(segment: &Segment, input: &Arc<[u8]>) -> bool {
        matches!(&segment.bytes, SourceBytes::Asset(bytes) if Arc::ptr_eq(bytes, input))
    }

    fn bytes(contents: &BufferProvenance, range: Range<u64>) -> Vec<Option<u8>> {
        range
            .map(|offset| {
                contents.segments.iter().find_map(|segment| {
                    (segment.start <= offset && offset < segment.end()).then(|| {
                        segment.bytes.bytes()
                            [segment.range.start + (offset - segment.start) as usize]
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
        let mut contents = prepared();
        for (target, source) in [(0..2, 3..5), (2..6, 5..9), (6..12, 9..15)] {
            contents.write_slice(target, &input, source).unwrap();
        }
        assert!(
            contents
                .segments
                .iter()
                .all(|segment| shares(segment, &input))
        );
        assert_eq!(contents.validate_indices(3, 8, u32::MAX), Ok(()));
        let copied = copy_range(&contents, 2..10, 4);
        assert_eq!(bytes(&copied, 4..12), bytes(&contents, 2..10));
        assert!(
            copied
                .segments
                .iter()
                .all(|segment| shares(segment, &input))
        );
        // A patch from another slice of the same allocation invalidates the
        // previous successful bounds check and uses that slice's exact bytes.
        contents.write_slice(4..8, &input, 15..19).unwrap();
        assert_eq!(
            contents.validate_indices(3, 8, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 9,
                vertex_count: 8
            })
        );
        contents.clear_range(1..2).unwrap();
        assert_eq!(
            contents.validate_indices(3, 10, u32::MAX),
            Err(IndexError::UnknownContents)
        );
    }

    #[test]
    fn copies_share_input_and_preserve_partial_known_contents_and_holes() {
        let upload: Arc<[u8]> = Arc::from([1u8, 2, 3, 4, 5, 6, 7, 8]);
        let mut source = prepared();
        source.write(0..8, Arc::clone(&upload));
        source.write(2..4, Arc::from([9u8, 10]));
        let mut target = prepared();
        target.write(0..16, Arc::from([99u8; 16]));
        source.clear_range(3..5).unwrap();
        target
            .replace(8..14, &copy_range(&source, 1..7, 8))
            .unwrap();
        assert_eq!(
            bytes(&target, 8..14),
            [Some(2), Some(9), None, None, Some(6), Some(7)]
        );
        assert_eq!(bytes(&target, 7..8), [Some(99)]);
        assert_eq!(bytes(&target, 14..16), [Some(99), Some(99)]);
        assert!(
            target
                .segments
                .iter()
                .any(|segment| shares(segment, &upload))
        );
        // A disjoint same-buffer copy snapshots its slices before overwriting.
        let copied = copy_range(&source, 0..8, 8);
        source.replace(8..16, &copied).unwrap();
        assert_eq!(bytes(&source, 0..8), bytes(&source, 8..16));
    }

    #[test]
    fn index_checks_span_shared_slices_and_enforce_both_bounds() {
        let maximum: u32 = (1 << 24) - 1;
        let mut contents = prepared();
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
        contents.clear_range(5..6).unwrap();
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
        let mut contents = prepared();
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
                    contents.clear_range(range).unwrap();
                }
                _ => {
                    // Snapshot before replacement, like a disjoint same-buffer
                    // GPU copy. Both initialized bytes and holes are copied.
                    let source = start % 48;
                    let target = 64 + source;
                    let values = expected[source..source + length].to_vec();
                    let copied = copy_range(
                        &contents,
                        source as u64..(source + length) as u64,
                        target as u64,
                    );
                    contents
                        .replace(target as u64..(target + length) as u64, &copied)
                        .unwrap();
                    expected[target..target + length].copy_from_slice(&values);
                }
            }
            assert_eq!(bytes(&contents, 0..128), expected, "update {step}");
        }
    }

    #[test]
    fn validation_reuses_content_maximum_across_vertex_and_device_bounds() {
        let mut contents = prepared();
        contents.write(0..8, Arc::from([0u8; 8]));
        for _ in 0..100 {
            assert_eq!(contents.validate_indices(2, 1, 0), Ok(()));
        }
        assert_eq!(contents.validation_scans, 1);
        let mut old_contents = snapshot(&contents);
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
        contents
            .replace(0..4, &copy_range(&old_contents, 0..4, 0))
            .unwrap();
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
        contents.clear_range(1..2).unwrap();
        assert_eq!(
            contents.validate_indices(1, 1, 0),
            Err(IndexError::UnknownContents)
        );
        contents.write(1..2, Arc::from([0u8]));
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
    }

    #[test]
    fn managed_mesh_has_no_payload_or_scans_and_rechecks_dynamic_bounds() {
        let mut contents = prepared();
        contents.write_mesh_indices(0..4, 12, 2);
        assert_eq!(
            contents.validate_indices(3, 3, 2),
            Err(IndexError::UnknownContents)
        );
        let mut unaccepted = snapshot(&contents);
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
        let mut mixed = prepared();
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
        let mut original = prepared();
        original.write_mesh_indices(0..12, 12, 2);
        let mut copied = prepared();
        copied
            .replace(0..12, &copy_range(&original, 0..12, 0))
            .unwrap();
        assert_eq!(copied.validate_indices(3, 3, 2), Ok(()));
        assert_eq!(copied.test_evidence(), (0, 0));
        let mut partial = copy_range(&original, 0..4, 0);
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
        let mut sparse = prepared();
        sparse.write_mesh_indices(4..12, 12, 2);
        sparse.write_mesh_indices(0..4, 12, 2);
        assert_eq!(
            sparse.validate_indices(3, 3, 2),
            Err(IndexError::UnknownContents)
        );
    }

    #[test]
    fn generic_alternating_vertex_bounds_share_one_content_summary() {
        let mut contents = prepared();
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

    #[test]
    fn capacity_failure_preserves_bytes_summary_and_backing_owners() {
        let input: Arc<[u8]> = Arc::from([0u8; 12]);
        let patch: Arc<[u8]> = Arc::from([1u8; 4]);
        let mut contents = BufferProvenance::with_capacity(2);
        contents.write_slice(0..12, &input, 0..12).unwrap();
        assert_eq!(contents.validate_indices(3, 1, 0), Ok(()));
        let retained = Arc::strong_count(&input);
        assert_eq!(
            contents.write_slice(4..8, &patch, 0..4),
            Err(ProvenanceCapacityError {
                required: 3,
                capacity: 2
            })
        );
        assert_eq!(Arc::strong_count(&input), retained);
        assert_eq!(Arc::strong_count(&patch), 1);
        assert_eq!(bytes(&contents, 0..12), [Some(0); 12]);
        assert_eq!(contents.validate_indices(3, 1, 0), Ok(()));
        assert_eq!(contents.validation_scans, 1);
        contents.reserve_segments(3);
        contents.write_slice(4..8, &patch, 0..4).unwrap();
        assert_eq!(
            bytes(&contents, 0..12),
            [
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(1),
                Some(1),
                Some(1),
                Some(1),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
            ]
        );
        assert!(matches!(
            contents.validate_indices(3, 1, 0),
            Err(IndexError::VertexBounds { .. })
        ));
        assert_eq!(contents.validation_scans, 2);
    }

    #[test]
    fn failed_replacement_and_copy_leave_destination_version_intact() {
        let input: Arc<[u8]> = Arc::from([0u8; 12]);
        let mut source = BufferProvenance::with_capacity(3);
        source.write_slice(0..4, &input, 0..4).unwrap();
        source.write_slice(8..12, &input, 8..12).unwrap();
        let mut target = BufferProvenance::with_capacity(1);
        target.write_mesh_indices(0..12, 12, 2);
        let failure = Err(ProvenanceCapacityError {
            required: 2,
            capacity: 1,
        });
        assert_eq!(source.copy_into(0..12, 0, &mut target), failure);
        assert_eq!(target.copy_from(&source), failure);
        assert_eq!(target.replace(0..12, &source), failure);
        assert_eq!(target.validate_indices(3, 3, 2), Ok(()));
        assert_eq!(target.test_evidence(), (0, 0));

        // Clearing a hole can itself need an extra retained edge segment.
        target.write_slice(0..12, &input, 0..12).unwrap();
        assert_eq!(target.validate_indices(3, 1, 0), Ok(()));
        assert_eq!(target.clear_range(4..8), failure);
        assert_eq!(bytes(&target, 0..12), [Some(0); 12]);
        assert_eq!(target.validate_indices(3, 1, 0), Ok(()));
        assert_eq!(target.validation_scans, 1);
    }

    #[test]
    fn prepared_churn_clear_and_empty_ranges_keep_metadata_storage() {
        // Separate backing owners model a live GraphPool. They remain alive
        // through mutation/reset so removing metadata cannot free byte storage.
        let input: Arc<[u8]> = Arc::from([0u8; 32]);
        let patch: Arc<[u8]> = Arc::from([1u8; 4]);
        let mut source = BufferProvenance::with_capacity(8);
        let mut target = BufferProvenance::with_capacity(8);
        let mut scratch = BufferProvenance::with_capacity(8);
        let pointers = [
            source.segments.as_ptr(),
            target.segments.as_ptr(),
            scratch.segments.as_ptr(),
        ];
        let capacities = [
            source.segment_capacity(),
            target.segment_capacity(),
            scratch.segment_capacity(),
        ];
        for _ in 0..100 {
            source.write_slice(0..16, &input, 0..16).unwrap();
            source.write_slice(4..8, &patch, 0..4).unwrap();
            source.clear_range(10..12).unwrap();
            source.copy_into(0..16, 16, &mut scratch).unwrap();
            target.copy_from(&source).unwrap();
            target.replace(16..32, &scratch).unwrap();
            assert_eq!(target.segments.len(), 8);
            // No-op empty writes/copies never manufacture empty segments.
            source.write_slice(5..5, &patch, 1..1).unwrap();
            source.clear_range(5..5).unwrap();
            source.copy_into(5..5, 0, &mut scratch).unwrap();
            assert!(scratch.segments.is_empty());
            source.clear();
            target.clear();
            scratch.clear();
            assert_eq!(
                [
                    source.segments.as_ptr(),
                    target.segments.as_ptr(),
                    scratch.segments.as_ptr()
                ],
                pointers
            );
            assert_eq!(
                [
                    source.segment_capacity(),
                    target.segment_capacity(),
                    scratch.segment_capacity()
                ],
                capacities
            );
        }
        assert_eq!(Arc::strong_count(&input), 1);
        assert_eq!(Arc::strong_count(&patch), 1);
    }
}
