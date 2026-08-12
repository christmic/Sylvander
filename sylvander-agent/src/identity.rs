//! Non-wire identities used inside one Agent execution.
//!
//! Runtime constructs these values from authenticated product identities. The
//! Agent keeps distinct types to prevent public API DTOs from becoming kernel
//! authority or persistence dependencies.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! execution_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            #[must_use]
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

execution_id!(
    AgentId,
    "Runtime-selected Agent identity for kernel-domain records."
);
execution_id!(
    SessionId,
    "Opaque execution correlation identity for kernel-domain records."
);
execution_id!(
    UserId,
    "Authenticated user identity for kernel-domain records."
);

impl UserId {
    /// Identity used only for trusted Runtime operations without a human actor.
    #[must_use]
    pub fn system() -> Self {
        Self("__system__".into())
    }
}
