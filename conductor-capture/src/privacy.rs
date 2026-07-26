// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Privacy level definitions and controls

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Privacy level for captured patterns
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "lowercase")]
pub enum PrivacyLevel {
    /// Public - anyone can download
    Public,

    /// Private - only owner can access (default)
    #[default]
    Private,

    /// Friends - approved users only
    Friends,

    /// Premium - paid access
    Premium,
}

impl PrivacyLevel {
    /// Check if this privacy level allows public access
    pub fn is_public(&self) -> bool {
        matches!(self, PrivacyLevel::Public)
    }

    /// Check if this privacy level requires authentication
    pub fn requires_auth(&self) -> bool {
        matches!(self, PrivacyLevel::Friends | PrivacyLevel::Premium)
    }

    /// Check if this privacy level allows monetization
    pub fn allows_monetization(&self) -> bool {
        matches!(self, PrivacyLevel::Premium)
    }
}

impl std::fmt::Display for PrivacyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivacyLevel::Public => write!(f, "public"),
            PrivacyLevel::Private => write!(f, "private"),
            PrivacyLevel::Friends => write!(f, "friends"),
            PrivacyLevel::Premium => write!(f, "premium"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_privacy_levels() {
        assert!(PrivacyLevel::Public.is_public());
        assert!(!PrivacyLevel::Private.is_public());

        assert!(PrivacyLevel::Friends.requires_auth());
        assert!(!PrivacyLevel::Public.requires_auth());

        assert!(PrivacyLevel::Premium.allows_monetization());
        assert!(!PrivacyLevel::Public.allows_monetization());
    }

    #[test]
    fn test_default() {
        assert_eq!(PrivacyLevel::default(), PrivacyLevel::Private);
    }
}
