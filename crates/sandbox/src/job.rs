use std::fmt;

use thiserror::Error;

/// Number of lowercase hexadecimal characters in a job identifier or nonce.
pub const IDENTIFIER_LENGTH: usize = 32;

macro_rules! hex_identifier {
    ($name:ident, $label:literal, $doc:literal) => {
        #[doc = $doc]
        ///
        /// The value is server-generated and restricted to lowercase
        /// hexadecimal so that it can be placed in a container name, a
        /// filesystem path, or an output sentinel without any escaping.
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Validates and constructs the identifier.
            ///
            /// # Errors
            ///
            /// Returns an [`IdentifierError`] when `value` is not exactly
            /// [`IDENTIFIER_LENGTH`] lowercase hexadecimal characters.
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate(&value, $label)?;
                Ok(Self(value))
            }

            /// Returns the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

hex_identifier!(
    JobId,
    "job id",
    "Identifies one playground run for its container name and job directory."
);
hex_identifier!(
    Nonce,
    "nonce",
    "Separates trusted output sections from anything the program prints."
);

/// A rejected job identifier or nonce.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IdentifierError {
    /// The value was not exactly [`IDENTIFIER_LENGTH`] characters.
    #[error("{label} must be exactly {expected} characters, got {length}")]
    WrongLength {
        label: &'static str,
        length: usize,
        expected: usize,
    },
    /// The value contained something other than lowercase hexadecimal.
    #[error("{label} must be lowercase hexadecimal")]
    NotHexadecimal { label: &'static str },
}

fn validate(value: &str, label: &'static str) -> Result<(), IdentifierError> {
    if value.len() != IDENTIFIER_LENGTH {
        return Err(IdentifierError::WrongLength {
            label,
            length: value.len(),
            expected: IDENTIFIER_LENGTH,
        });
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(IdentifierError::NotHexadecimal { label });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{IDENTIFIER_LENGTH, IdentifierError, JobId, Nonce};

    fn sample() -> String {
        "0123456789abcdef0123456789abcdef".to_owned()
    }

    #[test]
    fn well_formed_identifiers_are_accepted_and_preserved() {
        let value = sample();

        assert_eq!(JobId::new(value.clone()).unwrap().as_str(), value);
        assert_eq!(Nonce::new(value.clone()).unwrap().to_string(), value);
    }

    #[test]
    fn identifiers_of_the_wrong_length_are_rejected() {
        assert_eq!(
            JobId::new("abc").unwrap_err(),
            IdentifierError::WrongLength {
                label: "job id",
                length: 3,
                expected: IDENTIFIER_LENGTH,
            }
        );
        assert!(Nonce::new(String::new()).is_err());
    }

    #[test]
    fn shell_and_container_metacharacters_cannot_reach_an_identifier() {
        for candidate in [
            "0123456789ABCDEF0123456789abcdef", // uppercase
            "0123456789abcdef0123456789abcde;", // shell separator
            "0123456789abcdef0123456789abcd -", // argument-looking
            "../123456789abcdef0123456789abcd", // traversal
            "0123456789abcdef0123456789abcdeg", // non-hex letter
        ] {
            assert_eq!(
                JobId::new(candidate).unwrap_err(),
                IdentifierError::NotHexadecimal { label: "job id" },
                "expected {candidate:?} to be rejected"
            );
        }
    }
}
