use thiserror::Error;

use crate::limits::SandboxLimits;

/// Checks that submitted source is something the sandbox is willing to write to disk.
///
/// This rejects control characters that have no business in source text — the
/// record separator used by the output framing among them — so the only way a
/// run can emit one is by printing it at run time, where the per-run nonce
/// makes it harmless. See [`crate::framing`].
///
/// # Errors
///
/// Returns a [`SourceError`] describing the first violation found.
pub fn validate_source(source: &str, limits: &SandboxLimits) -> Result<(), SourceError> {
    if source.is_empty() {
        return Err(SourceError::Empty);
    }

    if source.len() > limits.max_source_bytes {
        return Err(SourceError::TooLarge {
            bytes: source.len(),
            maximum: limits.max_source_bytes,
        });
    }

    first_disallowed_control(source)
}

/// Checks that submitted standard input is within its bound and free of NUL bytes.
///
/// Standard input is deliberately more permissive than source: a program may
/// legitimately be fed arbitrary text, including escape sequences.
///
/// # Errors
///
/// Returns a [`SourceError`] describing the first violation found.
pub fn validate_stdin(stdin: &str, limits: &SandboxLimits) -> Result<(), SourceError> {
    if stdin.len() > limits.max_stdin_bytes {
        return Err(SourceError::TooLarge {
            bytes: stdin.len(),
            maximum: limits.max_stdin_bytes,
        });
    }

    match stdin.find('\0') {
        Some(index) => Err(SourceError::NulByte { index }),
        None => Ok(()),
    }
}

fn first_disallowed_control(source: &str) -> Result<(), SourceError> {
    for (index, character) in source.char_indices() {
        if character == '\0' {
            return Err(SourceError::NulByte { index });
        }

        if character.is_control() && !matches!(character, '\t' | '\n' | '\r') {
            return Err(SourceError::ControlCharacter { index, character });
        }
    }

    Ok(())
}

/// A rejected source or standard-input payload.
///
/// Variants carry positions and sizes only. They never carry the offending
/// text, so a caller may render them without echoing submitted content.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SourceError {
    /// The payload contained no bytes.
    #[error("source cannot be empty")]
    Empty,
    /// The payload exceeded its byte limit.
    #[error("payload is {bytes} bytes; maximum is {maximum}")]
    TooLarge { bytes: usize, maximum: usize },
    /// The payload contained a NUL byte.
    #[error("payload contains a NUL byte at offset {index}")]
    NulByte { index: usize },
    /// The payload contained a control character outside tab, newline, and carriage return.
    #[error("payload contains a control character at offset {index}")]
    ControlCharacter { index: usize, character: char },
}

#[cfg(test)]
mod tests {
    use super::{SourceError, validate_source, validate_stdin};
    use crate::limits::SandboxLimits;

    #[test]
    fn ordinary_source_with_tabs_and_newlines_is_accepted() {
        let limits = SandboxLimits::default();

        assert!(validate_source("Fn Main() {\r\n\tPrint(\"hi\")\n}\n", &limits).is_ok());
    }

    #[test]
    fn empty_source_is_rejected_but_empty_stdin_is_not() {
        let limits = SandboxLimits::default();

        assert_eq!(
            validate_source("", &limits).unwrap_err(),
            SourceError::Empty
        );
        assert!(validate_stdin("", &limits).is_ok());
    }

    #[test]
    fn the_size_limit_counts_bytes_rather_than_characters() {
        let limits = SandboxLimits {
            max_source_bytes: 8,
            ..SandboxLimits::default()
        };

        // Four characters, twelve bytes.
        assert_eq!(
            validate_source("日本語だ", &limits).unwrap_err(),
            SourceError::TooLarge {
                bytes: 12,
                maximum: 8,
            }
        );
        assert!(validate_source("12345678", &limits).is_ok());
    }

    #[test]
    fn nul_bytes_are_rejected_in_both_source_and_stdin() {
        let limits = SandboxLimits::default();

        assert_eq!(
            validate_source("ab\0cd", &limits).unwrap_err(),
            SourceError::NulByte { index: 2 }
        );
        assert_eq!(
            validate_stdin("ab\0cd", &limits).unwrap_err(),
            SourceError::NulByte { index: 2 }
        );
    }

    #[test]
    fn control_characters_including_the_framing_separator_are_rejected_in_source() {
        let limits = SandboxLimits::default();

        for (source, index, character) in [
            ("ok\u{1e}bad", 2, '\u{1e}'),
            ("ok\u{7}bad", 2, '\u{7}'),
            ("ok\u{7f}bad", 2, '\u{7f}'),
            ("ok\u{85}bad", 2, '\u{85}'),
        ] {
            assert_eq!(
                validate_source(source, &limits).unwrap_err(),
                SourceError::ControlCharacter { index, character },
                "expected {source:?} to be rejected"
            );
        }
    }

    #[test]
    fn stdin_may_carry_control_characters_a_program_might_legitimately_read() {
        let limits = SandboxLimits::default();

        assert!(validate_stdin("\u{1b}[31mred\u{1b}[0m\n", &limits).is_ok());
    }

    #[test]
    fn the_reported_offset_is_a_byte_offset_into_the_original_source() {
        let limits = SandboxLimits::default();

        assert_eq!(
            validate_source("日\u{1e}", &limits).unwrap_err(),
            SourceError::ControlCharacter {
                index: 3,
                character: '\u{1e}',
            }
        );
    }
}
