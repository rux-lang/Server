use std::fmt;

/// An operating system accepted by a manifest dependency's `TargetOS` allow-list.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TargetOs {
    Windows,
    Linux,
    MacOs,
    FreeBsd,
    OpenBsd,
    NetBsd,
    DragonFlyBsd,
    Illumos,
}

impl TargetOs {
    /// Parses one exact manifest spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Windows" => Some(Self::Windows),
            "Linux" => Some(Self::Linux),
            "MacOS" => Some(Self::MacOs),
            "FreeBSD" => Some(Self::FreeBsd),
            "OpenBSD" => Some(Self::OpenBsd),
            "NetBSD" => Some(Self::NetBsd),
            "DragonFlyBSD" => Some(Self::DragonFlyBsd),
            "Illumos" => Some(Self::Illumos),
            _ => None,
        }
    }

    /// Returns the exact spelling used by `Rux.toml` and registry JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::MacOs => "MacOS",
            Self::FreeBsd => "FreeBSD",
            Self::OpenBsd => "OpenBSD",
            Self::NetBsd => "NetBSD",
            Self::DragonFlyBsd => "DragonFlyBSD",
            Self::Illumos => "Illumos",
        }
    }
}

impl fmt::Display for TargetOs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
