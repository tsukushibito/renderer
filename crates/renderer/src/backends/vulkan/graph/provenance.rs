//! CPU-known buffer contents reference immutable upload input; GPU copies move
//! slice metadata, not bytes. Unknown holes remain unknown after partial copies.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(super) struct BufferProvenance {
    segments: BTreeMap<u64, Segment>,
    // One result bounds cache storage independently of the number of draw
    // variants. Cloning preserves the same immutable contents and valid result.
    validation: Option<IndexValidation>,
    #[cfg(test)]
    validation_scans: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct IndexBounds {
    count: u32,
    vertex_count: u64,
    maximum: u32,
}

#[derive(Clone, Copy)]
struct IndexValidation {
    bounds: IndexBounds,
    result: Result<(), IndexError>,
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
    /// `target` and `bytes` describe an already validated graph upload.
    pub(super) fn write(&mut self, target: Range<u64>, bytes: Arc<[u8]>) {
        debug_assert_eq!(target.end - target.start, bytes.len() as u64);
        let length = bytes.len();
        self.replace(
            target.clone(),
            Self {
                segments: BTreeMap::from([(
                    target.start,
                    Segment {
                        start: target.start,
                        bytes,
                        range: 0..length,
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
        Self {
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
    pub(super) fn replace(&mut self, target: Range<u64>, contents: Self) {
        // Mutation invalidates both successful and failed validation results.
        // Other clones still describe their old immutable contents.
        self.validation = None;
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
        let bounds = IndexBounds {
            count,
            vertex_count,
            maximum,
        };
        if let Some(validation) = self.validation
            && validation.bounds == bounds
        {
            return validation.result;
        }
        #[cfg(test)]
        {
            self.validation_scans += 1;
        }
        let result = self.scan_indices(bounds);
        self.validation = Some(IndexValidation { bounds, result });
        result
    }

    fn scan_indices(&self, bounds: IndexBounds) -> Result<(), IndexError> {
        let IndexBounds {
            count,
            vertex_count,
            maximum,
        } = bounds;
        let end = u64::from(count) * 4;
        if end == 0 {
            return Ok(());
        }
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
                    validate_index(u32::from_le_bytes(pending), vertex_count, maximum)?;
                    filled = 0;
                }
            }
            let (indices, tail) = bytes.as_chunks::<4>();
            for index in indices {
                validate_index(u32::from_le_bytes(*index), vertex_count, maximum)?;
            }
            if !tail.is_empty() {
                pending[..tail.len()].copy_from_slice(tail);
                filled = tail.len();
            }
            cursor = next;
            if cursor == end {
                debug_assert_eq!(filled, 0);
                return Ok(());
            }
        }
        if cursor == end {
            Ok(())
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
    fn validation_reuses_only_unchanged_contents_and_identical_bounds() {
        let mut contents = BufferProvenance::default();
        contents.write(0..8, Arc::from([0u8; 8]));
        for _ in 0..100 {
            assert_eq!(contents.validate_indices(2, 1, 0), Ok(()));
        }
        assert_eq!(contents.validation_scans, 1);
        let mut old_contents = contents.clone();
        // Different vertex bounds, Device limits, and index ranges cannot use
        // another draw's cached result, including when the old result was OK.
        assert!(matches!(
            contents.validate_indices(2, 0, 0),
            Err(IndexError::VertexBounds { .. })
        ));
        assert_eq!(contents.validation_scans, 2);
        assert_eq!(contents.validate_indices(1, 1, 0), Ok(()));
        assert_eq!(contents.validation_scans, 3);
        assert_eq!(contents.validate_indices(1, 1, 1), Ok(()));
        assert_eq!(contents.validation_scans, 4);

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
}
