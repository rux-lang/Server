use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

/// Maximum number of comparators accepted in one version range.
pub const VERSION_RANGE_MAX_COMPARATORS: usize = 32;

/// A strict Semantic Versioning 2.0.0 value.
///
/// Equality, hashing, and total ordering include build metadata. Use
/// [`Self::cmp_precedence`] when Semantic Versioning precedence is required.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SemanticVersion {
    source: String,
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
    build: Option<String>,
}

impl SemanticVersion {
    /// Parses a strict Semantic Versioning 2.0.0 value.
    ///
    /// # Errors
    ///
    /// Returns a [`SemanticVersionError`] when `value` is not a complete,
    /// valid semantic version.
    pub fn new(value: impl Into<String>) -> Result<Self, SemanticVersionError> {
        let source = value.into();
        let (major, minor, patch, prerelease, build) = {
            let parsed = parse_semantic_version(&source)?;
            (
                parsed.major,
                parsed.minor,
                parsed.patch,
                parsed.prerelease.map(str::to_owned),
                parsed.build.map(str::to_owned),
            )
        };

        Ok(Self {
            source,
            major,
            minor,
            patch,
            prerelease,
            build,
        })
    }

    /// Returns the original valid spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Returns the major version number.
    #[must_use]
    pub const fn major(&self) -> u64 {
        self.major
    }

    /// Returns the minor version number.
    #[must_use]
    pub const fn minor(&self) -> u64 {
        self.minor
    }

    /// Returns the patch version number.
    #[must_use]
    pub const fn patch(&self) -> u64 {
        self.patch
    }

    /// Returns the prerelease identifier without the leading `-`.
    #[must_use]
    pub fn prerelease(&self) -> Option<&str> {
        self.prerelease.as_deref()
    }

    /// Returns the build metadata without the leading `+`.
    #[must_use]
    pub fn build(&self) -> Option<&str> {
        self.build.as_deref()
    }

    /// Compares Semantic Versioning precedence, ignoring build metadata.
    #[must_use]
    pub fn cmp_precedence(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| compare_prerelease(self.prerelease(), other.prerelease()))
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

impl FromStr for SemanticVersion {
    type Err = SemanticVersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for SemanticVersion {
    type Error = SemanticVersionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for SemanticVersion {
    type Error = SemanticVersionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Ord for SemanticVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_precedence(other)
            .then_with(|| compare_build(self.build(), other.build()))
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A failure to parse a [`SemanticVersion`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticVersionError {
    /// The input was empty.
    Empty,
    /// A required numeric component was absent.
    MissingComponent {
        /// Stable component name: `major`, `minor`, or `patch`.
        component: &'static str,
        /// Byte offset at which the component was expected.
        index: usize,
    },
    /// A numeric component contained a leading zero.
    LeadingZero {
        /// Stable component name.
        component: &'static str,
        /// Byte offset of the leading zero.
        index: usize,
    },
    /// A numeric component exceeded `u64`.
    NumericOverflow {
        /// Stable component name.
        component: &'static str,
        /// Byte offset at which the component began.
        index: usize,
    },
    /// A character was not accepted at its location.
    InvalidCharacter {
        /// Stable section name.
        section: &'static str,
        /// UTF-8 byte offset of the character.
        index: usize,
        /// Rejected character.
        character: char,
    },
    /// A prerelease or build identifier was empty.
    EmptyIdentifier {
        /// Stable section name: `prerelease` or `build`.
        section: &'static str,
        /// Byte offset of the empty identifier.
        index: usize,
    },
    /// A numeric prerelease identifier contained a leading zero.
    LeadingZeroInPrerelease {
        /// Byte offset of the leading zero.
        index: usize,
    },
}

impl fmt::Display for SemanticVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("semantic version cannot be empty"),
            Self::MissingComponent { component, index } => {
                write!(
                    formatter,
                    "semantic version is missing {component} at byte {index}"
                )
            }
            Self::LeadingZero { component, index } => write!(
                formatter,
                "semantic version {component} has a leading zero at byte {index}"
            ),
            Self::NumericOverflow { component, index } => write!(
                formatter,
                "semantic version {component} exceeds u64 at byte {index}"
            ),
            Self::InvalidCharacter {
                section,
                index,
                character,
            } => write!(
                formatter,
                "semantic version {section} contains invalid character {character:?} at byte {index}"
            ),
            Self::EmptyIdentifier { section, index } => write!(
                formatter,
                "semantic version {section} contains an empty identifier at byte {index}"
            ),
            Self::LeadingZeroInPrerelease { index } => write!(
                formatter,
                "semantic version prerelease has a numeric identifier with a leading zero at byte {index}"
            ),
        }
    }
}

impl Error for SemanticVersionError {}

/// A Cargo-compatible intersection of semantic-version comparators.
#[derive(Clone, Debug)]
pub struct VersionRange {
    source: String,
    comparators: Vec<Comparator>,
}

impl VersionRange {
    /// Parses a Cargo-compatible version range.
    ///
    /// # Errors
    ///
    /// Returns a [`VersionRangeError`] when `value` does not satisfy the range
    /// grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, VersionRangeError> {
        let source = value.into();
        let comparators = parse_version_range(&source)?;
        Ok(Self {
            source,
            comparators,
        })
    }

    /// Returns the original valid spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Returns whether `version` satisfies every comparator in this range.
    #[must_use]
    pub fn matches(&self, version: &SemanticVersion) -> bool {
        self.comparators
            .iter()
            .all(|comparator| comparator.matches(version))
            && prerelease_is_compatible(&self.comparators, version)
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

impl FromStr for VersionRange {
    type Err = VersionRangeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for VersionRange {
    type Error = VersionRangeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for VersionRange {
    type Error = VersionRangeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A failure to parse a [`VersionRange`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionRangeError {
    /// The input was empty or contained only spaces.
    Empty,
    /// A comma was followed by no comparator.
    EmptyComparator {
        /// Byte offset at which a comparator was expected.
        index: usize,
    },
    /// A wildcard comparator was combined with another comparator.
    WildcardMustStandAlone {
        /// Byte offset of the wildcard.
        index: usize,
    },
    /// The comparator limit was exceeded.
    TooManyComparators {
        /// Maximum accepted comparator count.
        maximum: usize,
    },
    /// A comparator was malformed.
    InvalidComparator {
        /// UTF-8 byte offset at or nearest to the failure.
        index: usize,
        /// Stable reason for the failure.
        reason: &'static str,
    },
}

impl fmt::Display for VersionRangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("version range cannot be empty"),
            Self::EmptyComparator { index } => {
                write!(
                    formatter,
                    "version range is missing a comparator at byte {index}"
                )
            }
            Self::WildcardMustStandAlone { index } => write!(
                formatter,
                "version range wildcard at byte {index} must stand alone"
            ),
            Self::TooManyComparators { maximum } => write!(
                formatter,
                "version range has too many comparators; maximum is {maximum}"
            ),
            Self::InvalidComparator { index, reason } => write!(
                formatter,
                "version range contains an invalid comparator at byte {index}: {reason}"
            ),
        }
    }
}

impl Error for VersionRangeError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operator {
    Exact,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
    Tilde,
    Caret,
    Wildcard,
}

#[derive(Clone, Debug)]
struct Comparator {
    operator: Operator,
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    prerelease: Option<String>,
}

impl Comparator {
    fn matches(&self, version: &SemanticVersion) -> bool {
        match self.operator {
            Operator::Exact | Operator::Wildcard => self.matches_exact(version),
            Operator::Greater => self.matches_greater(version),
            Operator::GreaterOrEqual => {
                self.matches_exact(version) || self.matches_greater(version)
            }
            Operator::Less => self.matches_less(version),
            Operator::LessOrEqual => self.matches_exact(version) || self.matches_less(version),
            Operator::Tilde => self.matches_tilde(version),
            Operator::Caret => self.matches_caret(version),
        }
    }

    fn matches_exact(&self, version: &SemanticVersion) -> bool {
        version.major == self.major
            && self.minor.is_none_or(|minor| version.minor == minor)
            && self.patch.is_none_or(|patch| version.patch == patch)
            && compare_prerelease(version.prerelease(), self.prerelease.as_deref())
                == Ordering::Equal
    }

    fn matches_greater(&self, version: &SemanticVersion) -> bool {
        if version.major != self.major {
            return version.major > self.major;
        }
        let Some(minor) = self.minor else {
            return false;
        };
        if version.minor != minor {
            return version.minor > minor;
        }
        let Some(patch) = self.patch else {
            return false;
        };
        if version.patch != patch {
            return version.patch > patch;
        }
        compare_prerelease(version.prerelease(), self.prerelease.as_deref()) == Ordering::Greater
    }

    fn matches_less(&self, version: &SemanticVersion) -> bool {
        if version.major != self.major {
            return version.major < self.major;
        }
        let Some(minor) = self.minor else {
            return false;
        };
        if version.minor != minor {
            return version.minor < minor;
        }
        let Some(patch) = self.patch else {
            return false;
        };
        if version.patch != patch {
            return version.patch < patch;
        }
        compare_prerelease(version.prerelease(), self.prerelease.as_deref()) == Ordering::Less
    }

    fn matches_tilde(&self, version: &SemanticVersion) -> bool {
        version.major == self.major
            && self.minor.is_none_or(|minor| version.minor == minor)
            && self.patch.is_none_or(|patch| version.patch >= patch)
            && compare_prerelease(version.prerelease(), self.prerelease.as_deref())
                != Ordering::Less
    }

    fn matches_caret(&self, version: &SemanticVersion) -> bool {
        if version.major != self.major {
            return false;
        }
        let Some(minor) = self.minor else {
            return true;
        };
        let Some(patch) = self.patch else {
            return if self.major > 0 {
                version.minor >= minor
            } else {
                version.minor == minor
            };
        };

        let core_matches = if self.major > 0 {
            version.minor > minor || (version.minor == minor && version.patch >= patch)
        } else if minor > 0 {
            version.minor == minor && version.patch >= patch
        } else {
            version.minor == minor && version.patch == patch
        };

        core_matches
            && compare_prerelease(version.prerelease(), self.prerelease.as_deref())
                != Ordering::Less
    }
}

struct ParsedVersion<'a> {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<&'a str>,
    build: Option<&'a str>,
}

fn parse_semantic_version(value: &str) -> Result<ParsedVersion<'_>, SemanticVersionError> {
    if value.is_empty() {
        return Err(SemanticVersionError::Empty);
    }

    let suffix_index = value.find(['-', '+']).unwrap_or(value.len());
    let core = &value[..suffix_index];
    let (major, minor, patch) = parse_core(core)?;
    let (prerelease, build) = parse_suffix(value, suffix_index)?;

    Ok(ParsedVersion {
        major,
        minor,
        patch,
        prerelease,
        build,
    })
}

fn parse_core(value: &str) -> Result<(u64, u64, u64), SemanticVersionError> {
    let first_dot = value
        .find('.')
        .ok_or(SemanticVersionError::MissingComponent {
            component: "minor",
            index: value.len(),
        })?;
    let second_dot = value[first_dot + 1..]
        .find('.')
        .map(|index| first_dot + 1 + index)
        .ok_or(SemanticVersionError::MissingComponent {
            component: "patch",
            index: value.len(),
        })?;

    let major = parse_version_number(&value[..first_dot], "major", 0)?;
    let minor = parse_version_number(&value[first_dot + 1..second_dot], "minor", first_dot + 1)?;
    let patch = parse_version_number(&value[second_dot + 1..], "patch", second_dot + 1)?;
    Ok((major, minor, patch))
}

fn parse_version_number(
    value: &str,
    component: &'static str,
    index: usize,
) -> Result<u64, SemanticVersionError> {
    if value.is_empty() {
        return Err(SemanticVersionError::MissingComponent { component, index });
    }
    if let Some((offset, character)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit())
    {
        return Err(SemanticVersionError::InvalidCharacter {
            section: component,
            index: index + offset,
            character,
        });
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(SemanticVersionError::LeadingZero { component, index });
    }
    value
        .parse()
        .map_err(|_| SemanticVersionError::NumericOverflow { component, index })
}

fn parse_suffix(
    value: &str,
    suffix_index: usize,
) -> Result<(Option<&str>, Option<&str>), SemanticVersionError> {
    if suffix_index == value.len() {
        return Ok((None, None));
    }

    let suffix = &value[suffix_index..];
    if let Some(prerelease_and_build) = suffix.strip_prefix('-') {
        let build_offset = prerelease_and_build.find('+');
        let prerelease =
            build_offset.map_or(prerelease_and_build, |index| &prerelease_and_build[..index]);
        validate_identifiers(prerelease, "prerelease", suffix_index + 1, true)?;
        if let Some(index) = build_offset {
            let build = &prerelease_and_build[index + 1..];
            let build_index = suffix_index + index + 2;
            validate_identifiers(build, "build", build_index, false)?;
            Ok((Some(prerelease), Some(build)))
        } else {
            Ok((Some(prerelease), None))
        }
    } else {
        let build = &suffix[1..];
        validate_identifiers(build, "build", suffix_index + 1, false)?;
        Ok((None, Some(build)))
    }
}

fn validate_identifiers(
    value: &str,
    section: &'static str,
    base_index: usize,
    reject_numeric_leading_zero: bool,
) -> Result<(), SemanticVersionError> {
    if value.is_empty() {
        return Err(SemanticVersionError::EmptyIdentifier {
            section,
            index: base_index,
        });
    }

    let mut segment_start = 0;
    for (index, character) in value.char_indices() {
        if character == '.' {
            validate_identifier_segment(
                &value[segment_start..index],
                section,
                base_index + segment_start,
                reject_numeric_leading_zero,
            )?;
            segment_start = index + 1;
        } else if !(character.is_ascii_alphanumeric() || character == '-') {
            return Err(SemanticVersionError::InvalidCharacter {
                section,
                index: base_index + index,
                character,
            });
        }
    }

    validate_identifier_segment(
        &value[segment_start..],
        section,
        base_index + segment_start,
        reject_numeric_leading_zero,
    )
}

fn validate_identifier_segment(
    value: &str,
    section: &'static str,
    index: usize,
    reject_numeric_leading_zero: bool,
) -> Result<(), SemanticVersionError> {
    if value.is_empty() {
        return Err(SemanticVersionError::EmptyIdentifier { section, index });
    }
    if reject_numeric_leading_zero
        && value.len() > 1
        && value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(SemanticVersionError::LeadingZeroInPrerelease { index });
    }
    Ok(())
}

fn compare_prerelease(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => compare_identifier_lists(left, right, false),
    }
}

fn compare_build(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => compare_identifier_lists(left, right, true),
    }
}

fn compare_identifier_lists(left: &str, right: &str, allow_leading_zeros: bool) -> Ordering {
    let mut right_segments = right.split('.');
    for left_segment in left.split('.') {
        let Some(right_segment) = right_segments.next() else {
            return Ordering::Greater;
        };
        let ordering = compare_identifier(left_segment, right_segment, allow_leading_zeros);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    if right_segments.next().is_some() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn compare_identifier(left: &str, right: &str, allow_leading_zeros: bool) -> Ordering {
    let left_numeric = left.bytes().all(|byte| byte.is_ascii_digit());
    let right_numeric = right.bytes().all(|byte| byte.is_ascii_digit());
    match (left_numeric, right_numeric) {
        (true, true) if allow_leading_zeros => {
            let left_value = left.trim_start_matches('0');
            let right_value = right.trim_start_matches('0');
            left_value
                .len()
                .cmp(&right_value.len())
                .then_with(|| left_value.cmp(right_value))
                .then_with(|| left.len().cmp(&right.len()))
        }
        (true, true) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => left.cmp(right),
    }
}

fn prerelease_is_compatible(comparators: &[Comparator], version: &SemanticVersion) -> bool {
    version.prerelease().is_none()
        || comparators.iter().any(|comparator| {
            comparator.major == version.major
                && comparator.minor == Some(version.minor)
                && comparator.patch == Some(version.patch)
                && comparator.prerelease.is_some()
        })
}

fn parse_version_range(value: &str) -> Result<Vec<Comparator>, VersionRangeError> {
    let (trimmed, base_index) = trim_spaces_with_offset(value, 0);
    if trimmed.is_empty() {
        return Err(VersionRangeError::Empty);
    }
    if is_wildcard(trimmed) {
        return Ok(Vec::new());
    }

    let mut comparators = Vec::new();
    let mut start = 0;
    loop {
        let end = trimmed[start..]
            .find(',')
            .map_or(trimmed.len(), |index| start + index);
        let (part, part_index) = trim_spaces_with_offset(&trimmed[start..end], base_index + start);
        if part.is_empty() {
            return Err(VersionRangeError::EmptyComparator { index: part_index });
        }
        if is_wildcard(part) {
            return Err(VersionRangeError::WildcardMustStandAlone { index: part_index });
        }
        if comparators.len() == VERSION_RANGE_MAX_COMPARATORS {
            return Err(VersionRangeError::TooManyComparators {
                maximum: VERSION_RANGE_MAX_COMPARATORS,
            });
        }
        comparators.push(parse_comparator(part, part_index)?);

        if end == trimmed.len() {
            break;
        }
        start = end + 1;
        if start == trimmed.len() {
            return Err(VersionRangeError::EmptyComparator {
                index: base_index + start,
            });
        }
    }
    Ok(comparators)
}

fn parse_comparator(value: &str, base_index: usize) -> Result<Comparator, VersionRangeError> {
    let (explicit_operator, operator, rest) = parse_operator(value);
    let spaces = rest.len() - rest.trim_start_matches(' ').len();
    let version = &rest[spaces..];
    let version_index = base_index + value.len() - rest.len() + spaces;
    if version.is_empty() {
        return invalid_range(version_index, "missing partial version");
    }

    let parsed = parse_partial_version(version, version_index)?;
    let operator = if explicit_operator {
        operator
    } else if parsed.has_wildcard {
        Operator::Wildcard
    } else {
        Operator::Caret
    };

    Ok(Comparator {
        operator,
        major: parsed.major,
        minor: parsed.minor,
        patch: parsed.patch,
        prerelease: parsed.prerelease.map(str::to_owned),
    })
}

fn parse_operator(value: &str) -> (bool, Operator, &str) {
    for (spelling, operator) in [
        (">=", Operator::GreaterOrEqual),
        ("<=", Operator::LessOrEqual),
        ("=", Operator::Exact),
        (">", Operator::Greater),
        ("<", Operator::Less),
        ("~", Operator::Tilde),
        ("^", Operator::Caret),
    ] {
        if let Some(rest) = value.strip_prefix(spelling) {
            return (true, operator, rest);
        }
    }
    (false, Operator::Caret, value)
}

struct PartialVersion<'a> {
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    prerelease: Option<&'a str>,
    has_wildcard: bool,
}

fn parse_partial_version(
    value: &str,
    base_index: usize,
) -> Result<PartialVersion<'_>, VersionRangeError> {
    let suffix_index = value.find(['-', '+']).unwrap_or(value.len());
    let core = &value[..suffix_index];
    let parts = core.split('.').collect::<Vec<_>>();
    if parts.len() > 3 {
        let index = core
            .match_indices('.')
            .nth(2)
            .map_or(base_index, |(index, _)| base_index + index);
        return invalid_range(index, "too many numeric components");
    }
    if parts.first().is_none_or(|part| part.is_empty()) || is_wildcard(parts[0]) {
        return invalid_range(base_index, "major version must be a number");
    }

    let major = parse_range_number(parts[0], "major", base_index)?;
    let (minor, minor_wildcard) = parse_optional_range_part(&parts, 1, base_index, core)?;
    let (patch, patch_wildcard) = parse_optional_range_part(&parts, 2, base_index, core)?;
    if minor_wildcard && patch.is_some() {
        return invalid_range(base_index, "numeric component cannot follow a wildcard");
    }
    let has_wildcard = minor_wildcard || patch_wildcard;
    let (prerelease, _) = parse_range_suffix(value, suffix_index, patch, has_wildcard, base_index)?;

    Ok(PartialVersion {
        major,
        minor,
        patch,
        prerelease,
        has_wildcard,
    })
}

fn parse_optional_range_part(
    parts: &[&str],
    position: usize,
    base_index: usize,
    core: &str,
) -> Result<(Option<u64>, bool), VersionRangeError> {
    let Some(part) = parts.get(position) else {
        return Ok((None, false));
    };
    let index = component_index(core, position) + base_index;
    if part.is_empty() {
        return invalid_range(index, "numeric component cannot be empty");
    }
    if is_wildcard(part) {
        Ok((None, true))
    } else {
        parse_range_number(part, ["major", "minor", "patch"][position], index)
            .map(|number| (Some(number), false))
    }
}

fn parse_range_number(
    value: &str,
    component: &'static str,
    index: usize,
) -> Result<u64, VersionRangeError> {
    if let Some((offset, _)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit())
    {
        return invalid_range(
            index + offset,
            "numeric component contains an invalid character",
        );
    }
    if value.len() > 1 && value.starts_with('0') {
        return invalid_range(index, "numeric component has a leading zero");
    }
    value
        .parse()
        .map_err(|_| VersionRangeError::InvalidComparator {
            index,
            reason: match component {
                "major" => "major version exceeds u64",
                "minor" => "minor version exceeds u64",
                _ => "patch version exceeds u64",
            },
        })
}

fn parse_range_suffix(
    value: &str,
    suffix_index: usize,
    patch: Option<u64>,
    has_wildcard: bool,
    base_index: usize,
) -> Result<(Option<&str>, Option<&str>), VersionRangeError> {
    if suffix_index == value.len() {
        return Ok((None, None));
    }
    if patch.is_none() || has_wildcard {
        return invalid_range(
            base_index + suffix_index,
            "prerelease and build metadata require a complete version",
        );
    }

    let parsed = parse_suffix(value, suffix_index).map_err(|error| {
        let relative_index = semantic_error_index(&error);
        VersionRangeError::InvalidComparator {
            index: base_index + relative_index,
            reason: "invalid prerelease or build metadata",
        }
    })?;
    Ok(parsed)
}

fn semantic_error_index(error: &SemanticVersionError) -> usize {
    match error {
        SemanticVersionError::Empty => 0,
        SemanticVersionError::MissingComponent { index, .. }
        | SemanticVersionError::LeadingZero { index, .. }
        | SemanticVersionError::NumericOverflow { index, .. }
        | SemanticVersionError::InvalidCharacter { index, .. }
        | SemanticVersionError::EmptyIdentifier { index, .. }
        | SemanticVersionError::LeadingZeroInPrerelease { index } => *index,
    }
}

fn component_index(core: &str, position: usize) -> usize {
    if position == 0 {
        return 0;
    }
    core.match_indices('.')
        .nth(position - 1)
        .map_or(core.len(), |(index, _)| index + 1)
}

fn trim_spaces_with_offset(value: &str, base_index: usize) -> (&str, usize) {
    let leading = value.len() - value.trim_start_matches(' ').len();
    (value.trim_matches(' '), base_index + leading)
}

fn is_wildcard(value: &str) -> bool {
    matches!(value, "*" | "x" | "X")
}

fn invalid_range<T>(index: usize, reason: &'static str) -> Result<T, VersionRangeError> {
    Err(VersionRangeError::InvalidComparator { index, reason })
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::collections::HashSet;

    use super::{
        SemanticVersion, SemanticVersionError, VERSION_RANGE_MAX_COMPARATORS, VersionRange,
        VersionRangeError,
    };

    #[test]
    fn strict_versions_preserve_and_expose_every_component() {
        let version =
            SemanticVersion::new("12.34.56-alpha.1+linux.001").expect("version should be valid");

        assert_eq!(version.as_str(), "12.34.56-alpha.1+linux.001");
        assert_eq!(version.to_string(), "12.34.56-alpha.1+linux.001");
        assert_eq!(version.major(), 12);
        assert_eq!(version.minor(), 34);
        assert_eq!(version.patch(), 56);
        assert_eq!(version.prerelease(), Some("alpha.1"));
        assert_eq!(version.build(), Some("linux.001"));
    }

    #[test]
    fn strict_versions_accept_numeric_boundaries_and_identifier_characters() {
        for value in [
            "0.0.0",
            "18446744073709551615.18446744073709551615.18446744073709551615",
            "1.2.3-0",
            "1.2.3-alpha-beta.9",
            "1.2.3+001.A-Z",
        ] {
            assert!(
                SemanticVersion::new(value).is_ok(),
                "expected {value:?} to be valid"
            );
        }
    }

    #[test]
    fn strict_versions_reject_incomplete_leading_zero_and_overflow_values() {
        assert_eq!(
            SemanticVersion::new("").unwrap_err(),
            SemanticVersionError::Empty
        );
        assert!(matches!(
            SemanticVersion::new("1.2").unwrap_err(),
            SemanticVersionError::MissingComponent {
                component: "patch",
                index: 3
            }
        ));
        assert!(matches!(
            SemanticVersion::new("1.02.3").unwrap_err(),
            SemanticVersionError::LeadingZero {
                component: "minor",
                index: 2
            }
        ));
        assert!(matches!(
            SemanticVersion::new("18446744073709551616.0.0").unwrap_err(),
            SemanticVersionError::NumericOverflow {
                component: "major",
                index: 0
            }
        ));
    }

    #[test]
    fn strict_versions_reject_invalid_core_and_suffix_syntax() {
        for value in [
            "v1.2.3",
            "1.2.3 ",
            "1.2.3.4",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-alpha..1",
            "1.2.3-alpha_1",
            "1.2.3-01",
            "1.2.3-alpha+build+extra",
        ] {
            assert!(
                SemanticVersion::new(value).is_err(),
                "expected {value:?} to be invalid"
            );
        }
    }

    #[test]
    fn semantic_version_precedence_follows_the_specification() {
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ]
        .map(|value| SemanticVersion::new(value).expect("fixture should be valid"));

        for pair in ordered.windows(2) {
            assert_eq!(pair[0].cmp_precedence(&pair[1]), Ordering::Less);
        }
    }

    #[test]
    fn build_metadata_is_distinct_identity_but_equal_precedence() {
        let linux = SemanticVersion::new("1.2.3+linux").expect("version should be valid");
        let windows = SemanticVersion::new("1.2.3+windows").expect("version should be valid");
        let plain = SemanticVersion::new("1.2.3").expect("version should be valid");

        assert_ne!(linux, windows);
        assert_eq!(linux.cmp_precedence(&windows), Ordering::Equal);
        assert_eq!(linux.cmp_precedence(&plain), Ordering::Equal);
        assert_eq!(HashSet::from([linux.clone(), windows.clone()]).len(), 2);
        assert!(plain < linux);
        assert!(linux < windows);
    }

    #[test]
    fn ranges_preserve_source_and_accept_cargo_grammar() {
        for value in [
            "*",
            "x",
            "X",
            "1",
            "1.2",
            "1.2.3",
            "^1.2.3",
            "~1.2",
            "=1.2.3",
            ">= 1.2.3, < 2.0.0",
            "1.*",
            "1.x.*",
            "1.2.X",
            ">=1.2.3-alpha.1+ignored",
        ] {
            let range = VersionRange::new(value)
                .unwrap_or_else(|error| panic!("expected {value:?} to be valid: {error}"));
            assert_eq!(range.as_str(), value);
            assert_eq!(range.to_string(), value);
        }
    }

    #[test]
    fn ranges_reject_unsupported_and_malformed_syntax() {
        for value in [
            "",
            "   ",
            "*.*",
            "*, >=1",
            "1 || 2",
            "1.2 - 2.0",
            ">=1 <2",
            ">=",
            "1..2",
            "1.*.3",
            "1.2.3,",
            "1.2.3-alpha_1",
            "1.2-alpha",
            "01.2.3",
        ] {
            assert!(
                VersionRange::new(value).is_err(),
                "expected {value:?} to be invalid"
            );
        }
    }

    #[test]
    fn range_errors_expose_stable_kinds_and_byte_offsets() {
        assert_eq!(
            VersionRange::new("*, >=1").unwrap_err(),
            VersionRangeError::WildcardMustStandAlone { index: 0 }
        );
        assert_eq!(
            VersionRange::new(">=1.0 <2.0").unwrap_err(),
            VersionRangeError::InvalidComparator {
                index: 5,
                reason: "numeric component contains an invalid character"
            }
        );
        assert_eq!(
            VersionRange::new("1.2.3,  ").unwrap_err(),
            VersionRangeError::EmptyComparator { index: 6 }
        );
    }

    #[test]
    fn ranges_bound_comparator_count() {
        let maximum = std::iter::repeat_n(">=1", VERSION_RANGE_MAX_COMPARATORS)
            .collect::<Vec<_>>()
            .join(",");
        assert!(VersionRange::new(maximum).is_ok());

        let excessive = std::iter::repeat_n(">=1", VERSION_RANGE_MAX_COMPARATORS + 1)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            VersionRange::new(excessive).unwrap_err(),
            VersionRangeError::TooManyComparators {
                maximum: VERSION_RANGE_MAX_COMPARATORS
            }
        );
    }

    #[test]
    fn caret_ranges_follow_leftmost_nonzero_compatibility() {
        assert_matches("1.2.3", &["1.2.3", "1.9.9"], &["1.2.2", "2.0.0"]);
        assert_matches("^0.2.3", &["0.2.3", "0.2.99"], &["0.2.2", "0.3.0"]);
        assert_matches("^0.0.3", &["0.0.3"], &["0.0.2", "0.0.4"]);
        assert_matches("^1.2", &["1.2.0", "1.9.0"], &["1.1.9", "2.0.0"]);
        assert_matches("^0.0", &["0.0.0", "0.0.9"], &["0.1.0"]);
    }

    #[test]
    fn tilde_exact_wildcard_and_comparison_ranges_match_boundaries() {
        assert_matches("~1.2.3", &["1.2.3", "1.2.99"], &["1.2.2", "1.3.0"]);
        assert_matches("=1.2", &["1.2.0", "1.2.99"], &["1.1.9", "1.3.0"]);
        assert_matches("1.2.*", &["1.2.0", "1.2.99"], &["1.1.9", "1.3.0"]);
        assert_matches(">=1.2.3, <2.0.0", &["1.2.3", "1.9.9"], &["1.2.2", "2.0.0"]);
        assert_matches(">1.2", &["1.3.0", "2.0.0"], &["1.2.99"]);
        assert_matches("<=1.2", &["1.2.99", "0.9.0"], &["1.3.0"]);
    }

    #[test]
    fn prereleases_require_an_explicit_same_core_prerelease_comparator() {
        assert_matches("*", &["1.2.3"], &["1.2.3-alpha"]);
        assert_matches(">=1.2.3", &["1.2.3", "2.0.0"], &["2.0.0-alpha"]);
        assert_matches(
            ">=1.2.3-alpha",
            &["1.2.3-alpha", "1.2.3-beta", "1.2.3", "1.3.0"],
            &["1.2.2", "1.3.0-alpha"],
        );
    }

    #[test]
    fn build_metadata_does_not_affect_range_matching() {
        assert_matches(
            "=1.2.3+request-build",
            &["1.2.3", "1.2.3+candidate-build"],
            &["1.2.4"],
        );
    }

    fn assert_matches(range: &str, accepted: &[&str], rejected: &[&str]) {
        let range = VersionRange::new(range).expect("range fixture should be valid");
        for value in accepted {
            let version = SemanticVersion::new(*value).expect("version fixture should be valid");
            assert!(range.matches(&version), "expected {range} to match {value}");
        }
        for value in rejected {
            let version = SemanticVersion::new(*value).expect("version fixture should be valid");
            assert!(
                !range.matches(&version),
                "expected {range} not to match {value}"
            );
        }
    }
}
