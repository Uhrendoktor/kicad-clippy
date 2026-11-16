#![forbid(unsafe_code)]

//! Built-in KiCad schematic rules and project-profile execution.

mod config;
mod model;
mod rules;

pub use config::{
    AdjacencyContract, CommonNetContract, ComponentKind, ComponentMetadata, ComponentPurpose,
    ComponentRole, DesignGroup, DesignProfile, DocumentEvidence, PinPair, RuleConfig,
    RuleConfigError,
};
pub use rules::{available_rules, run_rules};
