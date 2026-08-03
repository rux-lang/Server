use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

/// Maximum number of ASCII bytes in an identity segment.
pub const IDENTITY_SEGMENT_MAX_LENGTH: usize = 64;

/// A display-preserving segment of a registry identity.
///
/// Segments contain ASCII alphanumeric characters separated by single `-` or
/// `_` characters. Equality, ordering, and hashing use an ASCII-lowercase form
/// in which `_` is folded to `-`.
#[derive(Clone, Debug)]
pub struct IdentitySegment {
    display: String,
    normalized: String,
}

impl IdentitySegment {
    /// Validates and constructs an identity segment.
    ///
    /// # Errors
    ///
    /// Returns an [`IdentitySegmentError`] when `value` does not satisfy the
    /// identity segment grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentitySegmentError> {
        let display = value.into();
        validate(&display)?;

        let mut normalized = String::with_capacity(display.len());
        for byte in display.bytes() {
            let byte = if byte == b'_' {
                b'-'
            } else {
                byte.to_ascii_lowercase()
            };
            normalized.push(char::from(byte));
        }

        Ok(Self {
            display,
            normalized,
        })
    }

    /// Returns the original, display-preserved segment.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.display
    }

    /// Returns the normalized collision key.
    #[must_use]
    pub fn normalized(&self) -> &str {
        &self.normalized
    }
}

impl fmt::Display for IdentitySegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display)
    }
}

impl FromStr for IdentitySegment {
    type Err = IdentitySegmentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for IdentitySegment {
    type Error = IdentitySegmentError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for IdentitySegment {
    type Error = IdentitySegmentError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl PartialEq for IdentitySegment {
    fn eq(&self, other: &Self) -> bool {
        self.normalized == other.normalized
    }
}

impl Eq for IdentitySegment {}

impl PartialOrd for IdentitySegment {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IdentitySegment {
    fn cmp(&self, other: &Self) -> Ordering {
        self.normalized.cmp(&other.normalized)
    }
}

impl Hash for IdentitySegment {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.normalized.hash(state);
    }
}

/// A failure to validate an [`IdentitySegment`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentitySegmentError {
    /// The segment was empty.
    Empty,
    /// The segment exceeded the maximum ASCII byte length.
    TooLong {
        /// Actual byte length.
        length: usize,
        /// Maximum accepted byte length.
        maximum: usize,
    },
    /// A character was outside the accepted ASCII grammar.
    InvalidCharacter {
        /// UTF-8 byte offset of the character.
        index: usize,
        /// Rejected character.
        character: char,
    },
    /// The segment began with a separator.
    LeadingSeparator {
        /// Rejected separator.
        character: char,
    },
    /// The segment ended with a separator.
    TrailingSeparator {
        /// UTF-8 byte offset of the separator.
        index: usize,
        /// Rejected separator.
        character: char,
    },
    /// Two separators appeared next to each other.
    AdjacentSeparators {
        /// Byte offset of the second separator.
        index: usize,
    },
}

impl fmt::Display for IdentitySegmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("identity segment cannot be empty"),
            Self::TooLong { length, maximum } => write!(
                formatter,
                "identity segment is {length} bytes; maximum is {maximum}"
            ),
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "identity segment contains invalid character {character:?} at byte {index}"
            ),
            Self::LeadingSeparator { character } => write!(
                formatter,
                "identity segment cannot start with separator {character:?}"
            ),
            Self::TrailingSeparator { index, character } => write!(
                formatter,
                "identity segment cannot end with separator {character:?} at byte {index}"
            ),
            Self::AdjacentSeparators { index } => write!(
                formatter,
                "identity segment contains adjacent separators at byte {index}"
            ),
        }
    }
}

impl Error for IdentitySegmentError {}

fn validate(value: &str) -> Result<(), IdentitySegmentError> {
    if value.is_empty() {
        return Err(IdentitySegmentError::Empty);
    }

    if value.len() > IDENTITY_SEGMENT_MAX_LENGTH {
        return Err(IdentitySegmentError::TooLong {
            length: value.len(),
            maximum: IDENTITY_SEGMENT_MAX_LENGTH,
        });
    }

    for (index, character) in value.char_indices() {
        if !(character.is_ascii_alphanumeric() || is_separator(character)) {
            return Err(IdentitySegmentError::InvalidCharacter { index, character });
        }
    }

    let first = char::from(value.as_bytes()[0]);
    if is_separator(first) {
        return Err(IdentitySegmentError::LeadingSeparator { character: first });
    }

    let last_index = value.len() - 1;
    let last = char::from(value.as_bytes()[last_index]);
    if is_separator(last) {
        return Err(IdentitySegmentError::TrailingSeparator {
            index: last_index,
            character: last,
        });
    }

    for (index, pair) in value.as_bytes().windows(2).enumerate() {
        if is_separator(char::from(pair[0])) && is_separator(char::from(pair[1])) {
            return Err(IdentitySegmentError::AdjacentSeparators { index: index + 1 });
        }
    }

    Ok(())
}

const fn is_separator(character: char) -> bool {
    matches!(character, '-' | '_')
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};
    use std::hash::{DefaultHasher, Hash, Hasher};

    use super::{IDENTITY_SEGMENT_MAX_LENGTH, IdentitySegment, IdentitySegmentError};

    #[test]
    fn valid_segments_preserve_display_and_expose_normalization() {
        let segment = IdentitySegment::new("Foo_Bar-9").expect("segment should be valid");

        assert_eq!(segment.as_str(), "Foo_Bar-9");
        assert_eq!(segment.to_string(), "Foo_Bar-9");
        assert_eq!(segment.normalized(), "foo-bar-9");
    }

    #[test]
    fn syntax_accepts_boundaries_and_supported_characters() {
        for value in [
            "a".to_owned(),
            "9".to_owned(),
            "9Lives".to_owned(),
            "alpha-beta_gamma".to_owned(),
            "a".repeat(IDENTITY_SEGMENT_MAX_LENGTH),
        ] {
            assert!(
                IdentitySegment::new(&value).is_ok(),
                "expected {value:?} to be valid"
            );
        }
    }

    #[test]
    fn syntax_rejects_empty_and_oversized_segments() {
        assert_eq!(
            IdentitySegment::new("").unwrap_err(),
            IdentitySegmentError::Empty
        );
        assert_eq!(
            IdentitySegment::new("a".repeat(IDENTITY_SEGMENT_MAX_LENGTH + 1)).unwrap_err(),
            IdentitySegmentError::TooLong {
                length: IDENTITY_SEGMENT_MAX_LENGTH + 1,
                maximum: IDENTITY_SEGMENT_MAX_LENGTH,
            }
        );
    }

    #[test]
    fn syntax_rejects_separators_at_boundaries() {
        assert_eq!(
            IdentitySegment::new("-alpha").unwrap_err(),
            IdentitySegmentError::LeadingSeparator { character: '-' }
        );
        assert_eq!(
            IdentitySegment::new("_alpha").unwrap_err(),
            IdentitySegmentError::LeadingSeparator { character: '_' }
        );
        assert_eq!(
            IdentitySegment::new("alpha-").unwrap_err(),
            IdentitySegmentError::TrailingSeparator {
                index: 5,
                character: '-',
            }
        );
        assert_eq!(
            IdentitySegment::new("alpha_").unwrap_err(),
            IdentitySegmentError::TrailingSeparator {
                index: 5,
                character: '_',
            }
        );
    }

    #[test]
    fn syntax_rejects_every_adjacent_separator_pair() {
        for value in ["alpha--beta", "alpha__beta", "alpha-_beta", "alpha_-beta"] {
            assert_eq!(
                IdentitySegment::new(value).unwrap_err(),
                IdentitySegmentError::AdjacentSeparators { index: 6 }
            );
        }
    }

    #[test]
    fn syntax_rejects_characters_outside_ascii_identity_grammar() {
        for (value, index, character) in [
            ("alpha beta", 5, ' '),
            ("alpha.beta", 5, '.'),
            ("alpha/beta", 5, '/'),
            ("alpha:beta", 5, ':'),
            ("café", 3, 'é'),
        ] {
            assert_eq!(
                IdentitySegment::new(value).unwrap_err(),
                IdentitySegmentError::InvalidCharacter { index, character }
            );
        }
    }

    #[test]
    fn normalized_collisions_have_equal_identity_semantics() {
        let spellings = ["Foo_Bar", "foo-bar", "FOO-BAR", "foo_bar"];
        let segments = spellings
            .map(|value| IdentitySegment::new(value).expect("collision spelling should be valid"));

        for segment in &segments[1..] {
            assert_eq!(segments[0], *segment);
            assert_eq!(segments[0].cmp(segment), std::cmp::Ordering::Equal);
            assert_eq!(hash(&segments[0]), hash(segment));
        }

        assert_eq!(segments.iter().cloned().collect::<HashSet<_>>().len(), 1);
        assert_eq!(segments.iter().cloned().collect::<BTreeSet<_>>().len(), 1);
    }

    #[test]
    fn distinct_normalized_segments_remain_distinct() {
        let separated = IdentitySegment::new("foo-bar").expect("segment should be valid");
        let joined = IdentitySegment::new("foobar").expect("segment should be valid");

        assert_ne!(separated, joined);
    }

    fn hash(value: &IdentitySegment) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
}
