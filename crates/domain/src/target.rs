use std::fmt;

/// An operating system accepted by a manifest dependency's `TargetOS` allow-list.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TargetOs {
    FreeBsd,
    Linux,
    MacOs,
    Windows,
}

impl TargetOs {
    /// Parses one exact manifest spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "FreeBSD" => Some(Self::FreeBsd),
            "Linux" => Some(Self::Linux),
            "macOS" => Some(Self::MacOs),
            "Windows" => Some(Self::Windows),
            _ => None,
        }
    }

    /// Returns the exact spelling used by `Rux.toml` and registry JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FreeBsd => "FreeBSD",
            Self::Linux => "Linux",
            Self::MacOs => "macOS",
            Self::Windows => "Windows",
        }
    }
}

impl fmt::Display for TargetOs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
