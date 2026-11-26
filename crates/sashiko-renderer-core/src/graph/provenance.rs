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
    // Exact range summaries sorted by first index and count. The prepared limit
    // makes exhaustion explicit instead of rescanning or growing during a frame.
    validations: Vec<IndexValidation>,
    // Managed meshes need no CPU copy. This private proof is installed only by
    // uploads carrying GeometryData's or MeshData's immutable bound, at the same
    // byte positions.
    mesh: Option<MeshIndexSummary>,
    #[cfg(test)]
    validation_scans: usize,
}

#[derive(Clone, Copy)]
struct IndexValidation {
    first_index: u32,
    count: u32,
    // None records an unknown byte/hole, independently of draw/Device bounds.
    maximum: Option<u32>,
}

fn index_bytes(first_index: u32, count: u32) -> Range<u64> {
    let start = u64::from(first_index) * 4;
    start..start + u64::from(count) * 4
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
            Self::Graph { graph, pass } => graph
                .data()
                .upload_data(&graph.data().passes[*pass].operation),
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
    Capacity(ProvenanceCapacityError),
    UnknownContents,
    VertexBounds { index: u32, vertex_count: u64 },
    DeviceLimit { index: u32, maximum: u32 },
}

impl BufferProvenance {
    pub(crate) fn with_capacity(segments: usize, index_prefixes: usize) -> Self {
        Self {
            segments: Vec::with_capacity(segments),
            validations: Vec::with_capacity(index_prefixes),
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

    #[cfg(test)]
    pub(crate) fn index_prefix_capacity(&self) -> usize {
        self.validations.capacity()
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

    fn require_prefix_capacity(&self, required: usize) -> Result<(), ProvenanceCapacityError> {
        if required > self.validations.capacity() {
            return Err(ProvenanceCapacityError {
                required,
                capacity: self.validations.capacity(),
            });
        }
        Ok(())
    }

    /// The external GraphPool owner keeps its allocations alive when pins drop.
    /// Clearing state does not discard the prepared segment storage.
    pub(crate) fn clear(&mut self) {
        self.segments.clear();
        self.validations.clear();
        self.mesh = None;
        #[cfg(test)]
        {
            self.validation_scans = 0;
        }
    }

    /// Copy an independent metadata version into already prepared storage. The
    /// immutable backing and range summaries can be shared without revalidation.
    pub(crate) fn copy_from(&mut self, source: &Self) -> Result<(), ProvenanceCapacityError> {
        self.require_capacity(source.segments.len())?;
        self.require_prefix_capacity(source.validations.len())?;
        self.segments.clear();
        self.segments.extend(source.segments.iter().cloned());
        self.validations.clear();
        self.validations.extend_from_slice(&source.validations);
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
        self.validations.clear();
        if target == (0..byte_len) {
            // A complete immutable replacement has no bytes from the previous
            // stream. Its new maximum may legitimately be smaller; retaining
            // the old maximum would reject valid replacement geometry.
            self.mesh = Some(MeshIndexSummary {
                byte_len,
                known_prefix: byte_len,
                maximum,
            });
            return;
        }
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
        let storage = graph.data();
        let bytes = storage.upload_data(&storage.passes[pass].operation);
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
        let validated_bytes = if source.start == destination {
            source.clone()
        } else {
            0..0
        };
        let validations = self.validations_within(validated_bytes);
        output.require_capacity(overlap.len())?;
        output.require_prefix_capacity(validations.clone().count())?;
        output.clear();
        output.validations.extend(validations);
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
        let validations = contents.validations_within(target.clone());
        self.require_capacity(self.replacement_len(&target, &overlap, contents.segments.len()))?;
        self.require_prefix_capacity(validations.clone().count())?;
        self.replace_segments(target, overlap, &contents.segments, contents.mesh);
        self.validations.extend(validations);
        Ok(())
    }

    // Exact range summaries stay valid only for bytes copied to the same
    // positions. A partially copied range cannot propagate its summary.
    fn validations_within(
        &self,
        bytes: Range<u64>,
    ) -> impl Iterator<Item = IndexValidation> + Clone + '_ {
        let first = self
            .validations
            .partition_point(|value| u64::from(value.first_index) * 4 < bytes.start);
        let end = first
            + self.validations[first..]
                .partition_point(|value| u64::from(value.first_index) * 4 < bytes.end);
        self.validations[first..end]
            .iter()
            .copied()
            .filter(move |value| index_bytes(value.first_index, value.count).end <= bytes.end)
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
        self.validations.clear();
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

    /// Check exactly the selected indices for a ranged draw. An immutable
    /// managed stream's full maximum can prove any contained range when that
    /// bound fits the selected vertex buffer and Device limit. A failed
    /// conservative bound does not prove a proper subrange is invalid.
    pub(crate) fn validate_index_range(
        &mut self,
        first_index: u32,
        count: u32,
        vertex_count: u64,
        maximum: u32,
    ) -> Result<(), IndexError> {
        if count == 0 {
            return Ok(());
        }
        if let Some(proof) = self.mesh {
            let bytes = index_bytes(first_index, count);
            if proof.known_prefix != proof.byte_len || bytes.end > proof.byte_len {
                return Err(IndexError::UnknownContents);
            }
            let bound = validate_index(proof.maximum, vertex_count, maximum);
            return if bytes == (0..proof.byte_len) || bound.is_ok() {
                bound
            } else {
                Err(IndexError::UnknownContents)
            };
        }
        self.validate_known_range(first_index, count, vertex_count, maximum)
    }

    fn validate_known_range(
        &mut self,
        first_index: u32,
        count: u32,
        vertex_count: u64,
        maximum: u32,
    ) -> Result<(), IndexError> {
        let content_maximum = match self
            .validations
            .binary_search_by_key(&(first_index, count), |v| (v.first_index, v.count))
        {
            Ok(index) => self.validations[index].maximum,
            Err(index) => {
                self.require_prefix_capacity(self.validations.len() + 1)
                    .map_err(IndexError::Capacity)?;
                #[cfg(test)]
                {
                    self.validation_scans += 1;
                }
                let maximum = self.scan_indices(index_bytes(first_index, count));
                self.validations.insert(
                    index,
                    IndexValidation {
                        first_index,
                        count,
                        maximum,
                    },
                );
                maximum
            }
        }
        .ok_or(IndexError::UnknownContents)?;
        validate_index(content_maximum, vertex_count, maximum)
    }

    fn scan_indices(&self, bytes: Range<u64>) -> Option<u32> {
        let end = bytes.end;
        let mut content_maximum = 0;
        let mut cursor = bytes.start;
        let mut pending = [0u8; 4];
        let mut filled = 0;
        for segment in &self.segments[self.overlap_indices(&bytes)] {
            if segment.start > cursor {
                return None;
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
                return Some(content_maximum);
            }
        }
        if cursor == end {
            Some(content_maximum)
        } else {
            None
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
        BufferProvenance::with_capacity(256, 64)
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
        assert_eq!(contents.validate_index_range(0, 3, 8, u32::MAX), Ok(()));
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
            contents.validate_index_range(0, 3, 8, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 9,
                vertex_count: 8
            })
        );
        contents.clear_range(1..2).unwrap();
        assert_eq!(
            contents.validate_index_range(0, 3, 10, u32::MAX),
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
            contents.validate_index_range(0, 3, u64::from(maximum) + 1, maximum),
            Ok(())
        );
        assert_eq!(
            contents.validate_index_range(0, 3, u64::from(maximum), maximum),
            Err(IndexError::VertexBounds {
                index: maximum,
                vertex_count: u64::from(maximum)
            })
        );
        assert_eq!(
            contents.validate_index_range(0, 3, u64::MAX, maximum - 1),
            Err(IndexError::DeviceLimit {
                index: maximum,
                maximum: maximum - 1
            })
        );
        contents.clear_range(5..6).unwrap();
        assert_eq!(
            contents.validate_index_range(0, 3, u64::MAX, u32::MAX),
            Err(IndexError::UnknownContents)
        );
        let full = Arc::from(u32::MAX.to_le_bytes());
        contents.write(0..4, full);
        assert_eq!(
            contents.validate_index_range(0, 1, u64::from(u32::MAX) + 1, u32::MAX),
            Ok(())
        );
        contents.write(0..4, Arc::from([0u8; 4]));
        assert_eq!(contents.validate_index_range(0, 1, 1, 0), Ok(()));
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
            assert_eq!(contents.validate_index_range(0, 2, 1, 0), Ok(()));
        }
        assert_eq!(contents.validation_scans, 1);
        let mut old_contents = snapshot(&contents);
        // Vertex and Device bounds are applied to the content maximum, so
        // changing them does not scan again. A different prefix is summarized
        // independently, including when the previous draw's bounds failed.
        assert!(matches!(
            contents.validate_index_range(0, 2, 0, 0),
            Err(IndexError::VertexBounds { .. })
        ));
        assert_eq!(contents.validation_scans, 1);
        assert_eq!(contents.validate_index_range(0, 1, 1, 0), Ok(()));
        assert_eq!(contents.validation_scans, 2);
        assert_eq!(contents.validate_index_range(0, 1, 1, 1), Ok(()));
        assert_eq!(contents.validation_scans, 2);

        // A partial upload invalidates the cache; clones keep their own old
        // contents and valid cache because all backing byte slices are immutable.
        contents.write(0..1, Arc::from([2u8]));
        assert_eq!(
            contents.validate_index_range(0, 1, 1, 1),
            Err(IndexError::VertexBounds {
                index: 2,
                vertex_count: 1
            })
        );
        assert_eq!(old_contents.validate_index_range(0, 2, 1, 0), Ok(()));
        assert_eq!(old_contents.validation_scans, 1);
        assert_eq!(
            contents.validate_index_range(0, 1, 3, 1),
            Err(IndexError::DeviceLimit {
                index: 2,
                maximum: 1
            })
        );
        // Copy replacement and unknown holes also invalidate prior failures.
        contents
            .replace(0..4, &copy_range(&old_contents, 0..4, 0))
            .unwrap();
        assert_eq!(contents.validate_index_range(0, 1, 1, 0), Ok(()));
        contents.clear_range(1..2).unwrap();
        assert_eq!(
            contents.validate_index_range(0, 1, 1, 0),
            Err(IndexError::UnknownContents)
        );
        contents.write(1..2, Arc::from([0u8]));
        assert_eq!(contents.validate_index_range(0, 1, 1, 0), Ok(()));
    }

    #[test]
    fn managed_mesh_has_no_payload_or_scans_and_rechecks_dynamic_bounds() {
        let mut contents = prepared();
        contents.write_mesh_indices(0..4, 12, 2);
        assert_eq!(
            contents.validate_index_range(0, 3, 3, 2),
            Err(IndexError::UnknownContents)
        );
        let mut unaccepted = snapshot(&contents);
        contents.write_mesh_indices(4..12, 12, 2);
        for draw in 0..32 {
            assert_eq!(contents.validate_index_range(0, 3, 3 + draw % 2, 2), Ok(()));
        }
        assert_eq!(contents.test_evidence(), (0, 0));
        assert_eq!(
            unaccepted.validate_index_range(0, 3, 3, 2),
            Err(IndexError::UnknownContents),
            "preparing another snapshot must not publish its accepted contents"
        );
        assert!(matches!(
            contents.validate_index_range(0, 3, 2, 2),
            Err(IndexError::VertexBounds { .. })
        ));
        assert!(matches!(
            contents.validate_index_range(0, 3, 3, 1),
            Err(IndexError::DeviceLimit { .. })
        ));
        assert_eq!(
            contents.validate_index_range(0, 1, 3, 2),
            Ok(()),
            "a successful whole-mesh bound also proves a contained prefix"
        );
        assert_eq!(contents.test_evidence(), (0, 0));
        // Replacing a stream with another mesh of the same byte length must
        // never lower the bound while old chunks can still contribute bytes.
        let mut mixed = prepared();
        mixed.write_mesh_indices(0..4, 12, 7);
        mixed.write_mesh_indices(4..12, 12, 2);
        assert_eq!(mixed.validate_index_range(0, 3, 8, 7), Ok(()));
        assert_eq!(
            mixed.validate_index_range(0, 3, 3, 7),
            Err(IndexError::VertexBounds {
                index: 7,
                vertex_count: 3
            })
        );
    }

    #[test]
    fn full_immutable_index_replacement_uses_new_bounds_without_retaining_payload() {
        let mut contents = prepared();
        contents.write_mesh_indices(0..12, 12, 2);
        let mut old = snapshot(&contents);
        contents.write_mesh_indices(0..12, 12, 0);
        assert_eq!(contents.validate_index_range(0, 3, 1, 0), Ok(()));
        assert_eq!(contents.test_evidence(), (0, 0));
        assert_eq!(
            old.validate_index_range(0, 3, 1, 0),
            Err(IndexError::VertexBounds {
                index: 2,
                vertex_count: 1
            }),
            "a replacement does not change already prepared snapshots"
        );
        contents.write_mesh_indices(0..12, 12, 3);
        assert_eq!(
            contents.validate_index_range(0, 3, 4, 2),
            Err(IndexError::DeviceLimit {
                index: 3,
                maximum: 2
            })
        );
        assert_eq!(contents.validate_index_range(0, 3, 4, 3), Ok(()));
        assert_eq!(contents.test_evidence(), (0, 0));
    }

    #[test]
    fn managed_mesh_copy_and_mutation_have_explicit_proof_boundaries() {
        let mut original = prepared();
        original.write_mesh_indices(0..12, 12, 2);
        let mut copied = prepared();
        copied
            .replace(0..12, &copy_range(&original, 0..12, 0))
            .unwrap();
        assert_eq!(copied.validate_index_range(0, 3, 3, 2), Ok(()));
        assert_eq!(copied.test_evidence(), (0, 0));
        let mut partial = copy_range(&original, 0..4, 0);
        assert_eq!(
            partial.validate_index_range(0, 1, 3, 2),
            Err(IndexError::UnknownContents)
        );
        copied.write(4..8, Arc::from(0u32.to_le_bytes()));
        assert_eq!(
            copied.validate_index_range(0, 3, 3, 2),
            Err(IndexError::UnknownContents),
            "arbitrary writes cannot reuse the unmodified mesh proof"
        );
        assert_eq!(
            original.validate_index_range(0, 3, 3, 2),
            Ok(()),
            "immutable old snapshots keep their own proof"
        );
        copied.write(0..12, Arc::from([0u8; 12]));
        assert_eq!(
            copied.validate_index_range(0, 3, 1, 0),
            Ok(()),
            "a complete generic replacement re-establishes byte provenance"
        );
        let mut sparse = prepared();
        sparse.write_mesh_indices(4..12, 12, 2);
        sparse.write_mesh_indices(0..4, 12, 2);
        assert_eq!(
            sparse.validate_index_range(0, 3, 3, 2),
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
            assert_eq!(
                contents.validate_index_range(0, 3, 3 + draw % 2, u32::MAX),
                Ok(())
            );
        }
        assert_eq!(contents.validation_scans, 1);
    }

    #[test]
    fn alternating_index_counts_keep_exact_prefix_guarantees_until_mutation() {
        let mut contents = prepared();
        contents.write(
            0..36,
            [0u32, 1, 0, 9, 0, 2, 12, 0, 1]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
                .into(),
        );
        // A failed draw still establishes the content maximum; draw bounds are
        // independent. A large prefix must not overestimate a shorter prefix.
        assert_eq!(
            contents.validate_index_range(0, 9, 1, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 12,
                vertex_count: 1
            })
        );
        for _ in 0..32 {
            for (count, maximum) in [(3, 1), (6, 9), (9, 12), (6, 9), (3, 1)] {
                assert_eq!(
                    contents.validate_index_range(0, count, u64::from(maximum) + 1, maximum),
                    Ok(())
                );
            }
        }
        assert_eq!(contents.validation_scans, 3);
        let mut old = snapshot(&contents);
        contents.write(0..1, Arc::from([2u8]));
        assert_eq!(
            contents.validate_index_range(0, 3, 2, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 2,
                vertex_count: 2
            })
        );
        assert_eq!(contents.validation_scans, 4);
        assert_eq!(old.validate_index_range(0, 3, 2, 1), Ok(()));
        assert_eq!(old.validate_index_range(0, 9, 13, 12), Ok(()));
        assert_eq!(old.validation_scans, 3);
    }

    #[test]
    fn prefix_capacity_failure_preserves_prior_guarantees_and_destination_version() {
        let input: Arc<[u8]> = Arc::from([0u8; 36]);
        let mut contents = BufferProvenance::with_capacity(1, 2);
        contents.write_slice(0..36, &input, 0..36).unwrap();
        for count in [3, 6] {
            assert_eq!(contents.validate_index_range(0, count, 1, 0), Ok(()));
        }
        assert_eq!(
            contents.validate_index_range(0, 9, 1, 0),
            Err(IndexError::Capacity(ProvenanceCapacityError {
                required: 3,
                capacity: 2
            }))
        );
        assert_eq!(
            contents.validation_scans, 2,
            "exhaustion must not scan or evict"
        );
        for count in [6, 3] {
            assert_eq!(contents.validate_index_range(0, count, 1, 0), Ok(()));
        }
        assert_eq!(contents.validation_scans, 2);

        let mut destination = BufferProvenance::with_capacity(1, 1);
        destination.write_mesh_indices(0..12, 12, 7);
        let failure = Err(ProvenanceCapacityError {
            required: 2,
            capacity: 1,
        });
        assert_eq!(destination.copy_from(&contents), failure);
        assert_eq!(contents.copy_into(0..36, 0, &mut destination), failure);
        assert_eq!(destination.replace(0..36, &contents), failure);
        assert_eq!(destination.validate_index_range(0, 3, 8, 7), Ok(()));
        assert_eq!(destination.test_evidence(), (0, 0));

        let mut managed_only = BufferProvenance::with_capacity(0, 0);
        assert_eq!(managed_only.validate_index_range(0, 0, 0, 0), Ok(()));
        managed_only.write_mesh_indices(0..12, 12, 2);
        assert_eq!(managed_only.validate_index_range(0, 3, 3, 2), Ok(()));
    }

    #[test]
    fn positional_prefix_copies_reuse_summaries_but_shifted_copies_do_not() {
        let mut source = prepared();
        source.write(
            0..12,
            [0u32, 1, 4]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
                .into(),
        );
        assert_eq!(source.validate_index_range(0, 2, 2, 1), Ok(()));
        assert_eq!(source.validate_index_range(0, 3, 5, 4), Ok(()));
        let mut copied = copy_range(&source, 0..12, 0);
        assert_eq!(copied.validate_index_range(0, 3, 5, 4), Ok(()));
        assert_eq!(copied.validate_index_range(0, 2, 2, 1), Ok(()));
        assert_eq!(copied.validation_scans, 0);
        let mut destination = prepared();
        destination.replace(0..12, &copied).unwrap();
        assert_eq!(destination.validate_index_range(0, 2, 2, 1), Ok(()));
        assert_eq!(destination.validation_scans, 0);

        let shifted = copy_range(&source, 0..12, 4);
        assert!(shifted.validations.is_empty());
        let mut suffix = copy_range(&source, 4..12, 0);
        assert_eq!(
            suffix.validate_index_range(0, 2, 2, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 4,
                vertex_count: 2
            })
        );
        assert_eq!(suffix.validation_scans, 1);

        source.clear_range(5..6).unwrap();
        assert_eq!(
            source.validate_index_range(0, 3, 5, 4),
            Err(IndexError::UnknownContents)
        );
        let mut hole = copy_range(&source, 0..12, 0);
        assert_eq!(
            hole.validate_index_range(0, 3, 5, 4),
            Err(IndexError::UnknownContents)
        );
        assert_eq!(hole.validation_scans, 0);
        hole.write(5..6, Arc::from([0u8]));
        assert_eq!(hole.validate_index_range(0, 3, 5, 4), Ok(()));
        assert_eq!(hole.validation_scans, 1);
    }

    #[test]
    fn nonzero_index_range_uses_only_its_own_bytes_and_revalidates_on_write() {
        let mut contents = BufferProvenance::with_capacity(4, 2);
        // The selected triangle begins after uninitialized data. Its largest
        // index straddles two separate uploads.
        contents.write(12..17, Arc::from([0u8, 0, 0, 0, 1]));
        contents.write(17..24, Arc::from([1u8, 0, 0, 2, 0, 0, 0]));
        contents.write(24..28, Arc::from(1024u32.to_le_bytes()));
        assert_eq!(contents.validate_index_range(3, 3, 258, 257), Ok(()));
        assert_eq!(
            contents.validate_index_range(4, 3, 258, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 1024,
                vertex_count: 258,
            })
        );
        assert_eq!(contents.validate_index_range(3, 3, 258, 257), Ok(()));
        assert_eq!(contents.validation_scans, 2);
        let mut previous = snapshot(&contents);
        contents.write(17..18, Arc::from([2u8]));
        assert_eq!(
            contents.validate_index_range(3, 3, 258, u32::MAX),
            Err(IndexError::VertexBounds {
                index: 513,
                vertex_count: 258,
            })
        );
        assert_eq!(contents.validation_scans, 3);
        assert_eq!(previous.validate_index_range(3, 3, 258, 257), Ok(()));
        assert_eq!(previous.validation_scans, 2);
    }

    #[test]
    fn positional_index_range_copy_inherits_only_contained_summaries() {
        let mut source = prepared();
        source.write(4..24, Arc::from([0u8; 20]));
        for (first_index, count) in [(1, 2), (2, 4), (3, 2), (3, 3), (4, 1)] {
            assert_eq!(
                source.validate_index_range(first_index, count, 1, 0),
                Ok(())
            );
        }
        let copied = copy_range(&source, 12..20, 12);
        let mut destination = prepared();
        destination.replace(12..20, &copied).unwrap();
        assert_eq!(destination.validate_index_range(3, 2, 1, 0), Ok(()));
        assert_eq!(destination.validate_index_range(4, 1, 1, 0), Ok(()));
        assert_eq!(destination.validation_scans, 0);
        assert_eq!(
            destination.validate_index_range(3, 3, 1, 0),
            Err(IndexError::UnknownContents)
        );
        assert_eq!(destination.validation_scans, 1);
    }

    #[test]
    fn packed_mesh_proves_ranged_draws_without_payload_or_range_slots() {
        let mut contents = BufferProvenance::with_capacity(0, 0);
        contents.write_mesh_indices(0..24, 48, 6);
        assert_eq!(
            contents.validate_index_range(3, 3, 7, 6),
            Err(IndexError::UnknownContents)
        );
        contents.write_mesh_indices(24..48, 48, 8);
        assert_eq!(contents.validate_index_range(0, 3, 9, 8), Ok(()));
        assert_eq!(contents.validate_index_range(3, 3, 9, 8), Ok(()));
        assert_eq!(contents.validate_index_range(9, 3, 9, 8), Ok(()));
        assert_eq!(
            contents.validate_index_range(3, 3, 9, 7),
            Err(IndexError::UnknownContents)
        );
        assert_eq!(
            contents.validate_index_range(9, 4, 9, 8),
            Err(IndexError::UnknownContents)
        );
        assert_eq!(contents.test_evidence(), (0, 0));
        let mut revised = BufferProvenance::with_capacity(0, 1);
        revised.copy_from(&contents).unwrap();
        revised.clear_range(14..15).unwrap();
        assert_eq!(
            revised.validate_index_range(3, 3, 9, 8),
            Err(IndexError::UnknownContents)
        );
        assert_eq!(contents.validate_index_range(3, 3, 9, 8), Ok(()));
    }

    #[test]
    fn capacity_failure_preserves_bytes_summary_and_backing_owners() {
        let input: Arc<[u8]> = Arc::from([0u8; 12]);
        let patch: Arc<[u8]> = Arc::from([1u8; 4]);
        let mut contents = BufferProvenance::with_capacity(2, 4);
        contents.write_slice(0..12, &input, 0..12).unwrap();
        assert_eq!(contents.validate_index_range(0, 3, 1, 0), Ok(()));
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
        assert_eq!(contents.validate_index_range(0, 3, 1, 0), Ok(()));
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
            contents.validate_index_range(0, 3, 1, 0),
            Err(IndexError::VertexBounds { .. })
        ));
        assert_eq!(contents.validation_scans, 2);
    }

    #[test]
    fn failed_replacement_and_copy_leave_destination_version_intact() {
        let input: Arc<[u8]> = Arc::from([0u8; 12]);
        let mut source = BufferProvenance::with_capacity(3, 4);
        source.write_slice(0..4, &input, 0..4).unwrap();
        source.write_slice(8..12, &input, 8..12).unwrap();
        let mut target = BufferProvenance::with_capacity(1, 4);
        target.write_mesh_indices(0..12, 12, 2);
        let failure = Err(ProvenanceCapacityError {
            required: 2,
            capacity: 1,
        });
        assert_eq!(source.copy_into(0..12, 0, &mut target), failure);
        assert_eq!(target.copy_from(&source), failure);
        assert_eq!(target.replace(0..12, &source), failure);
        assert_eq!(target.validate_index_range(0, 3, 3, 2), Ok(()));
        assert_eq!(target.test_evidence(), (0, 0));

        // Clearing a hole can itself need an extra retained edge segment.
        target.write_slice(0..12, &input, 0..12).unwrap();
        assert_eq!(target.validate_index_range(0, 3, 1, 0), Ok(()));
        assert_eq!(target.clear_range(4..8), failure);
        assert_eq!(bytes(&target, 0..12), [Some(0); 12]);
        assert_eq!(target.validate_index_range(0, 3, 1, 0), Ok(()));
        assert_eq!(target.validation_scans, 1);
    }

    #[test]
    fn prepared_churn_clear_and_empty_ranges_keep_metadata_storage() {
        // Separate backing owners model a live GraphPool. They remain alive
        // through mutation/reset so removing metadata cannot free byte storage.
        let input: Arc<[u8]> = Arc::from([0u8; 32]);
        let patch: Arc<[u8]> = Arc::from([1u8; 4]);
        let mut source = BufferProvenance::with_capacity(8, 4);
        let mut target = BufferProvenance::with_capacity(8, 4);
        let mut scratch = BufferProvenance::with_capacity(8, 4);
        let pointers = [
            source.segments.as_ptr(),
            target.segments.as_ptr(),
            scratch.segments.as_ptr(),
        ];
        let prefix_pointers = [
            source.validations.as_ptr(),
            target.validations.as_ptr(),
            scratch.validations.as_ptr(),
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
            assert_eq!(source.validate_index_range(0, 1, 1, 0), Ok(()));
            assert_eq!(
                source.validate_index_range(0, 2, u64::MAX, u32::MAX),
                Ok(())
            );
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
            assert_eq!(
                [
                    source.validations.as_ptr(),
                    target.validations.as_ptr(),
                    scratch.validations.as_ptr()
                ],
                prefix_pointers
            );
            assert_eq!(source.index_prefix_capacity(), 4);
            assert_eq!(target.index_prefix_capacity(), 4);
            assert_eq!(scratch.index_prefix_capacity(), 4);
        }
        assert_eq!(Arc::strong_count(&input), 1);
        assert_eq!(Arc::strong_count(&patch), 1);
    }
}
