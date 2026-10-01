//! Plugin layer: Agent Plugins 1.0.0 manifests, path containment and
//! component discovery.

pub mod diagnostics {
    //! Diagnostics shared across the plugin layer.

    use serde::{Deserialize, Serialize};

    /// Severity of a plugin diagnostic.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum DiagLevel {
        Error,
        Warning,
        Info,
    }

    /// One diagnosis about a plugin package or component.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Diagnostic {
        pub level: DiagLevel,
        pub target: String,
        pub message: String,
    }
}

pub use diagnostics::{DiagLevel, Diagnostic};

pub mod layout;
pub mod manifest;
pub mod marketplace;
pub mod path;

pub use marketplace::MarketplaceRecord;

#[cfg(test)]
mod tests;
