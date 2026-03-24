use similar::{ChangeTag, TextDiff};

use super::InlineSegment;

/// Check if a string contains meaningful (non-whitespace) content.
fn has_meaningful_content(s: &str) -> bool {
    s.chars().any(|c| !c.is_whitespace())
}

/// Compute word-level diff segments for a pair of modified lines.
/// Returns Some((old_segments, new_segments)) if word-level highlighting is useful,
/// or None if the lines are too different to benefit from it.
pub fn compute_word_diff(
    old_text: &str,
    new_text: &str,
) -> Option<(Vec<InlineSegment>, Vec<InlineSegment>)> {
    let diff = TextDiff::configure().diff_unicode_words(old_text, new_text);

    let mut old_segments = Vec::new();
    let mut new_segments = Vec::new();
    let mut unchanged_len = 0usize;

    for change in diff.iter_all_changes() {
        let text = change.value().to_string();
        match change.tag() {
            ChangeTag::Equal => {
                if has_meaningful_content(&text) {
                    unchanged_len += text.trim().len();
                }
                old_segments.push(InlineSegment {
                    text: text.clone(),
                    emphasized: false,
                });
                new_segments.push(InlineSegment {
                    text,
                    emphasized: false,
                });
            }
            ChangeTag::Delete => {
                old_segments.push(InlineSegment {
                    text,
                    emphasized: true,
                });
            }
            ChangeTag::Insert => {
                new_segments.push(InlineSegment {
                    text,
                    emphasized: true,
                });
            }
        }
    }

    let old_trimmed_len = old_text.trim().len();
    let new_trimmed_len = new_text.trim().len();
    let total_len = old_trimmed_len.max(new_trimmed_len);

    // Only show word-level diff if at least 20% of content is unchanged
    const MIN_UNCHANGED_RATIO: f64 = 0.20;
    if total_len == 0 || (unchanged_len as f64 / total_len as f64) < MIN_UNCHANGED_RATIO {
        return None;
    }

    Some((old_segments, new_segments))
}
