//! Local compatibility wrapper for the review-input contract in the next
//! jterm_core release.
//!
//! jterm4 exact-pins a published core revision, so keep the size and visual
//! spoofing checks here until that release can be advanced without depending on
//! unpublished workspace state.

use std::fmt;

pub const MAX_REVIEW_INPUT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewInputError {
    Empty,
    TooLarge,
    ControlCharacter,
    VisualSpoof,
}

impl fmt::Display for ReviewInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("the command is empty"),
            Self::TooLarge => write!(
                formatter,
                "the command exceeds the {MAX_REVIEW_INPUT_BYTES}-byte review limit"
            ),
            Self::ControlCharacter => formatter
                .write_str("the command contains a line break, NUL, or terminal control character"),
            Self::VisualSpoof => formatter.write_str(
                "the command contains an invisible or bidirectional formatting character",
            ),
        }
    }
}

pub fn contains_visual_spoof(text: &str) -> bool {
    text.chars().any(is_visual_spoof_character)
}

/// Multiline command recall permits structural newline/tab controls and lets
/// the PTY encoder defang other controls. Reject only non-control characters
/// whose visual presentation is ambiguous before that normalization.
pub fn contains_noncontrol_visual_spoof(text: &str) -> bool {
    text.chars()
        .any(|ch| !ch.is_control() && is_visual_spoof_character(ch))
}

/// Render untrusted terminal metadata without letting controls, bidi marks or
/// default-ignorable characters alter the surrounding UI. The returned value
/// is always valid UTF-8 and never exceeds `max_bytes`.
pub(crate) fn safe_inline_display(text: &str, max_bytes: usize) -> String {
    safe_display(text, max_bytes, false)
}

/// Display a command or document fragment while preserving only structural
/// newline/tab controls. All other terminal and visual formatting controls are
/// made explicit as replacement glyphs.
pub(crate) fn safe_multiline_display(text: &str, max_bytes: usize) -> String {
    safe_display(text, max_bytes, true)
}

fn safe_display(text: &str, max_bytes: usize, multiline: bool) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let mut output = String::with_capacity(text.len().min(max_bytes));
    let mut truncated = false;
    for ch in text.chars() {
        let rendered = if multiline && matches!(ch, '\n' | '\t') {
            ch
        } else if ch.is_control() || is_visual_spoof_character(ch) {
            '\u{fffd}'
        } else {
            ch
        };
        if output.len().saturating_add(rendered.len_utf8()) > max_bytes {
            truncated = true;
            break;
        }
        output.push(rendered);
    }
    if truncated && max_bytes >= '…'.len_utf8() {
        while output.len().saturating_add('…'.len_utf8()) > max_bytes {
            if output.pop().is_none() {
                break;
            }
        }
        output.push('…');
    }
    output
}

/// Public jsh session/execution identifiers use this exact grammar. Rejecting
/// rather than normalizing avoids collisions between two distinct wire IDs.
pub(crate) fn valid_jsh_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn is_visual_spoof_character(ch: char) -> bool {
    // Unicode 17 Default_Ignorable_Code_Point ranges, plus non-ASCII
    // whitespace. Keep the reserved FFF0 and supplementary tag ranges whole
    // so future format assignments fail closed without a release lag.
    (ch.is_whitespace() && ch != ' ')
        || matches!(
            ch,
                '\u{00ad}'
                    | '\u{034f}'
                    | '\u{061c}'
                    | '\u{115f}'..='\u{1160}'
                    | '\u{17b4}'..='\u{17b5}'
                    | '\u{180b}'..='\u{180f}'
                    | '\u{200b}'..='\u{200f}'
                    | '\u{2028}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{3164}'
                    | '\u{fe00}'..='\u{fe0f}'
                    | '\u{feff}'
                    | '\u{ffa0}'
                    | '\u{fff0}'..='\u{fff8}'
                    | '\u{1bca0}'..='\u{1bca3}'
                    | '\u{1d173}'..='\u{1d17a}'
                    | '\u{e0000}'..='\u{e0fff}'
        )
}

pub fn validate(text: &str) -> Result<&str, ReviewInputError> {
    if text.trim().is_empty() {
        return Err(ReviewInputError::Empty);
    }
    if text.len() > MAX_REVIEW_INPUT_BYTES {
        return Err(ReviewInputError::TooLarge);
    }
    if text.chars().any(char::is_control) {
        return Err(ReviewInputError::ControlCharacter);
    }
    if contains_visual_spoof(text) {
        return Err(ReviewInputError::VisualSpoof);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_text_is_single_line_bounded_and_visually_unambiguous() {
        assert_eq!(validate("echo safe"), Ok("echo safe"));
        assert_eq!(
            validate("echo\nunsafe"),
            Err(ReviewInputError::ControlCharacter)
        );
        assert_eq!(
            validate("echo safe\u{202e}txt"),
            Err(ReviewInputError::VisualSpoof)
        );
        assert_eq!(
            validate("echo safe\u{00a0}txt"),
            Err(ReviewInputError::VisualSpoof)
        );
        assert_eq!(
            validate("echo emoji\u{fe0f}"),
            Err(ReviewInputError::VisualSpoof)
        );
        assert_eq!(
            validate("echo reserved\u{fff0}mark"),
            Err(ReviewInputError::VisualSpoof)
        );
        assert_eq!(
            validate("echo tag\u{e0080}mark"),
            Err(ReviewInputError::VisualSpoof)
        );
        assert_eq!(
            validate(&"x".repeat(MAX_REVIEW_INPUT_BYTES + 1)),
            Err(ReviewInputError::TooLarge)
        );
    }

    #[test]
    fn display_text_is_bounded_and_makes_terminal_metadata_unambiguous() {
        assert_eq!(
            safe_inline_display("safe\n\u{202e}\u{fe0f}name", 64),
            "safe���name"
        );
        assert_eq!(
            safe_multiline_display("safe\n\t\u{202e}\u{fff0}\u{e01f0}name", 64),
            "safe\n\t���name"
        );
        assert_eq!(safe_inline_display("abcdefgh", 6), "abc…");
        assert!(safe_inline_display(&"界".repeat(100), 32).len() <= 32);
        // Unicode explicitly excludes Egyptian layout and interlinear
        // annotation controls from Default_Ignorable_Code_Point.
        assert!(!is_visual_spoof_character('\u{13430}'));
        assert!(!is_visual_spoof_character('\u{fff9}'));
    }

    #[test]
    fn jsh_ids_use_the_exact_public_wire_grammar() {
        assert!(valid_jsh_id("session_12-agent"));
        assert!(!valid_jsh_id(""));
        assert!(!valid_jsh_id(" padded"));
        assert!(!valid_jsh_id("session/12"));
        assert!(!valid_jsh_id(&"x".repeat(129)));
    }
}
