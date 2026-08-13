//! Minimal Agent-domain projection of an authenticated User Profile.
//!
//! Runtime owns the complete revisioned product profile and maps only fields
//! needed for prompt composition into this value. Timestamps, export shape,
//! administration metadata, and JSON Schema remain outside the Agent kernel.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyClass {
    Personal,
    Sensitive,
    Restricted,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClassifiedPreference<T> {
    pub value: T,
    pub privacy_class: PrivacyClass,
}

impl<T> fmt::Debug for ClassifiedPreference<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassifiedPreference")
            .field("value", &"[REDACTED]")
            .field("privacy_class", &self.privacy_class)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDetail {
    Concise,
    Balanced,
    Detailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunicationTone {
    Direct,
    Warm,
    Formal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessibilityPreferences {
    pub screen_reader_optimized: bool,
    pub reduce_motion: bool,
    pub high_contrast: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct UserProfileData {
    pub preferred_language: Option<ClassifiedPreference<String>>,
    pub locale: Option<ClassifiedPreference<String>>,
    pub response_detail: Option<ClassifiedPreference<ResponseDetail>>,
    pub communication_tone: Option<ClassifiedPreference<CommunicationTone>>,
    pub accessibility: Option<ClassifiedPreference<AccessibilityPreferences>>,
    pub constraints: Vec<ClassifiedPreference<String>>,
}

impl fmt::Debug for UserProfileData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileData")
            .field("preferences", &"[REDACTED]")
            .field("constraint_count", &self.constraints.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct UserProfileSnapshot {
    pub revision: u64,
    pub profile: UserProfileData,
    pub do_not_learn: bool,
}

impl fmt::Debug for UserProfileSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileSnapshot")
            .field("revision", &self.revision)
            .field("profile", &"[REDACTED]")
            .field("do_not_learn", &self.do_not_learn)
            .finish()
    }
}
