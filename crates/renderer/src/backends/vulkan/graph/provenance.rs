//! CPU-known buffer contents reference immutable upload input; GPU copies move
//! slice metadata, not bytes. Unknown holes remain unknown after partial copies.

use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(super) struct BufferProvenance(Vec<Segment>);

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

#[derive(Debug, PartialEq, Eq)]
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
            Self(vec![Segment {
                start: target.start,
                bytes,
                range: 0..length,
            }]),
        );
    }

    /// Return shared known slices at their destination positions. The source and
    /// destination ranges are graph-validated; gaps are deliberately preserved.
    /// Obtain this before replacing the destination to support disjoint self-copy.
    pub(super) fn copied(&self, source: Range<u64>, destination: u64) -> Self {
        Self(
            self.0
                .iter()
                .filter_map(|segment| {
                    let start = source.start.max(segment.start);
                    let end = source.end.min(segment.end());
                    (start < end).then(|| Segment {
                        start: destination + (start - source.start),
                        bytes: Arc::clone(&segment.bytes),
                        range: segment.range.start + (start - segment.start) as usize
                            ..segment.range.start + (end - segment.start) as usize,
                    })
                })
                .collect(),
        )
    }

    /// Overwrite the whole target, including unknown portions of a GPU copy.
    pub(super) fn replace(&mut self, target: Range<u64>, contents: Self) {
        let mut next = Vec::with_capacity(self.0.len() + contents.0.len() + 1);
        for segment in self.0.drain(..) {
            if segment.end() <= target.start || segment.start >= target.end {
                next.push(segment);
                continue;
            }
            if segment.start < target.start {
                let count = (target.start - segment.start) as usize;
                next.push(Segment {
                    start: segment.start,
                    bytes: Arc::clone(&segment.bytes),
                    range: segment.range.start..segment.range.start + count,
                });
            }
            if segment.end() > target.end {
                let skip = (target.end - segment.start) as usize;
                next.push(Segment {
                    start: target.end,
                    bytes: segment.bytes,
                    range: segment.range.start + skip..segment.range.end,
                });
            }
        }
        next.extend(contents.0);
        next.sort_by_key(|segment| segment.start);
        self.0 = next;
    }

    /// Validate little-endian u32 indices without reconstructing a byte buffer.
    /// Partial writes may split an index across multiple backing slices.
    pub(super) fn validate_indices(
        &self,
        count: u32,
        vertex_count: u64,
        maximum: u32,
    ) -> Result<(), IndexError> {
        let end = u64::from(count) * 4;
        let mut cursor = 0;
        let mut pending = [0u8; 4];
        let mut filled = 0;
        for segment in &self.0 {
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
                contents.0.iter().find_map(|segment| {
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
                .0
                .iter()
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
}
