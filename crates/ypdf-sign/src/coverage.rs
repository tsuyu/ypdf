//! What the `/ByteRange` actually covers.
//!
//! This is where most real-world signature fraud lives, and none of it is
//! cryptographic. The signature is over the bytes the `/ByteRange` names, and
//! nothing forces those to be *all* the bytes: a file can carry a perfectly
//! valid signature over a small honest region and a page of something else in
//! the part nobody signed. So the ranges are checked as carefully as the
//! digest is.

use lopdf::Dictionary;

use crate::model::{Coverage, Note};

/// The two halves of a file a signature covers, and what is between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// Offset and length pairs, in file order.
    pub spans: Vec<(usize, usize)>,
}

impl ByteRange {
    /// Read `/ByteRange [a b c d …]`.
    pub fn read(signature: &Dictionary) -> Option<Self> {
        let values = signature.get(b"ByteRange").ok()?.as_array().ok()?;
        if values.len() < 4 || values.len() % 2 != 0 {
            return None;
        }
        let numbers: Option<Vec<usize>> = values
            .iter()
            .map(|value| match value {
                lopdf::Object::Integer(n) => usize::try_from(*n).ok(),
                lopdf::Object::Real(n) if *n >= 0.0 => Some(*n as usize),
                _ => None,
            })
            .collect();
        let numbers = numbers?;

        let spans: Vec<(usize, usize)> = numbers
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .collect();
        Some(Self { spans })
    }

    /// The bytes the signature was computed over.
    ///
    /// Returns `None` when a span reaches past the end of the file, which is
    /// itself a finding: a signature cannot cover bytes that are not there.
    pub fn extract(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut signed = Vec::new();
        for (start, length) in &self.spans {
            let end = start.checked_add(*length)?;
            signed.extend_from_slice(bytes.get(*start..end)?);
        }
        Some(signed)
    }

    /// Total bytes covered.
    #[must_use]
    pub fn signed_bytes(&self) -> usize {
        self.spans.iter().map(|(_, length)| length).sum()
    }

    /// The gap between the first span and the second: where `/Contents` sits.
    #[must_use]
    pub fn gap(&self) -> Option<(usize, usize)> {
        let (first_start, first_length) = *self.spans.first()?;
        let (second_start, _) = *self.spans.get(1)?;
        let gap_start = first_start.checked_add(first_length)?;
        let gap_length = second_start.checked_sub(gap_start)?;
        Some((gap_start, gap_length))
    }

    /// The last byte covered.
    #[must_use]
    pub fn end(&self) -> usize {
        self.spans
            .iter()
            .map(|(start, length)| start.saturating_add(*length))
            .max()
            .unwrap_or(0)
    }
}

/// Bytes of slack allowed at the end of a file.
///
/// A signed file ends at `%%EOF`, and producers disagree about the newline
/// after it. More than this is content, not punctuation.
const TRAILING_SLACK: usize = 8;

/// Check a byte range against the file it came from.
///
/// Returns the coverage and everything worth saying about it.
pub fn assess(range: &ByteRange, contents_len: usize, file: &[u8]) -> (Coverage, Vec<Note>) {
    let mut notes = Vec::new();
    let total = file.len();
    let signed = range.signed_bytes();

    if range.spans.first().is_some_and(|(start, _)| *start != 0) {
        notes.push(Note::new(
            "SIG_RANGE_SKIPS_START",
            "the signature does not start at the beginning of the file, so the header and \
             anything before its first span are unsigned",
        ));
    }

    // The hole must hold the signature and nothing else. `/Contents` is a hex
    // string, so its size in the file is two characters per byte plus the
    // angle brackets; anything more is bytes nobody signed sitting inside the
    // region a reader will never show.
    if let Some((_, gap)) = range.gap() {
        let occupied = contents_len.saturating_mul(2).saturating_add(2);
        if gap > occupied {
            notes.push(Note::new(
                "SIG_GAP_LARGER_THAN_SIGNATURE",
                format!(
                    "the unsigned gap the signature sits in is {gap} bytes and the signature \
                     itself needs {occupied}; the remaining {} bytes are covered by nothing",
                    gap - occupied
                ),
            ));
        }
    }

    if range.end() > total {
        notes.push(Note::new(
            "SIG_RANGE_PAST_END",
            "the byte range reaches past the end of the file",
        ));
    }

    let coverage = if signed + range.gap().map_or(0, |(_, gap)| gap) + TRAILING_SLACK >= total {
        Coverage::WholeFile
    } else {
        notes.push(Note::new(
            "SIG_COVERS_EARLIER_REVISION",
            format!(
                "{} byte(s) were added after this signature; it says nothing about them",
                total.saturating_sub(signed + range.gap().map_or(0, |(_, gap)| gap))
            ),
        ));
        Coverage::PartOfFile { signed, total }
    };

    (coverage, notes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use lopdf::{Object, dictionary};

    use super::*;

    fn range(values: Vec<i64>) -> ByteRange {
        let dict = dictionary! {
            "ByteRange" => values.into_iter().map(Object::Integer).collect::<Vec<_>>(),
        };
        ByteRange::read(&dict).expect("reads")
    }

    #[test]
    fn a_range_reads_as_pairs_and_extracts_both_halves() {
        let bytes = b"HEAD<SIGNATURE>TAIL".to_vec();
        let range = range(vec![0, 4, 15, 4]);
        assert_eq!(range.extract(&bytes).expect("extracts"), b"HEADTAIL");
        assert_eq!(range.signed_bytes(), 8);
        assert_eq!(range.gap(), Some((4, 11)));
    }

    #[test]
    fn a_range_reaching_past_the_end_extracts_nothing() {
        // Better than a short read: a signature over bytes that do not exist
        // must not silently verify over the ones that do.
        let range = range(vec![0, 4, 15, 4000]);
        assert!(range.extract(b"HEAD<SIGNATURE>TAIL").is_none());
    }

    #[test]
    fn a_signature_over_the_whole_file_says_so() {
        let file = b"HEAD<0000000000>TAIL".to_vec();
        let range = range(vec![0, 4, 16, 4]);
        let (coverage, notes) = assess(&range, 5, &file);
        assert_eq!(coverage, Coverage::WholeFile);
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn content_appended_after_signing_is_reported() {
        let mut file = b"HEAD<0000000000>TAIL".to_vec();
        file.extend_from_slice(b"AND A WHOLE EXTRA PAGE OF SOMETHING ELSE");
        let range = range(vec![0, 4, 16, 4]);
        let (coverage, notes) = assess(&range, 5, &file);

        assert!(matches!(coverage, Coverage::PartOfFile { .. }));
        assert!(
            notes
                .iter()
                .any(|n| n.code == "SIG_COVERS_EARLIER_REVISION")
        );
    }

    #[test]
    fn a_gap_bigger_than_the_signature_is_reported() {
        // The classic trick: leave room in the unsigned hole and put something
        // in it. The digest still matches, because none of it is covered.
        let file = vec![b'x'; 400];
        let range = range(vec![0, 4, 300, 100]);
        let (_, notes) = assess(&range, 20, &file);
        assert!(
            notes
                .iter()
                .any(|n| n.code == "SIG_GAP_LARGER_THAN_SIGNATURE"),
            "{notes:?}"
        );
    }

    #[test]
    fn a_range_that_does_not_start_at_zero_is_reported() {
        let file = vec![b'x'; 100];
        let range = range(vec![10, 4, 20, 76]);
        let (_, notes) = assess(&range, 5, &file);
        assert!(notes.iter().any(|n| n.code == "SIG_RANGE_SKIPS_START"));
    }
}
