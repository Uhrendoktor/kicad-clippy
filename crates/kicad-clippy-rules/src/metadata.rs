use std::{collections::{BTreeMap, BTreeSet}, fs, path::Path};

use kicad_clippy_core::{Diagnostic, Severity};
use kicad_clippy_parser::SchematicDocument;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::generic::SchematicFacts;

/// Profile enforcement mode.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    /// Validate only metadata that is present.
    #[default]
    Standard,
    /// Require explicit metadata for every placed component.
    Strict,
}

/// Supported component families.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Capacitor, Resistor, Diode, Inductor, Transistor, Connector,
    IntegratedCircuit, Fuse, FerriteBead, Crystal, Switch, Other,
}

/// Extensible component role, such as decoupling, bulk, pull_up, clamp, or load_switch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ComponentRole(pub String);

/// Pin-specific intent for a decoupling capacitor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecouplingIntent {
    /// Reference and pin number, e.g. U1.21.
    pub target_pin: String,
    /// Capacitor terminal connected to the target pin.
    pub target_terminal: String,
    /// Capacitor terminal connected to the return network.
    pub return_terminal: String,
    /// Expected return net, commonly GND.
    pub return_net: String,
    /// Stable cluster identifier for capacitors serving the same target.
    pub cluster: String,
}

/// Declared purpose and engineering constraints for one component.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ComponentIntent {
    /// Component family.
    pub kind: ComponentKind,
    /// Circuit function category.
    pub role: ComponentRole,
    /// Concise description of the component's electrical function.
    pub function: Option<String>,
    /// Reason for selecting this implementation/value/topology.
    pub rationale: Option<String>,
    /// Datasheet, application note, design requirement, or review reference.
    pub evidence: Option<String>,
    /// Decoupling-specific pin and terminal contract.
    pub decoupling: Option<DecouplingIntent>,
    /// Optional minimum voltage rating in volts.
    pub voltage_rating_min_v: Option<f64>,
    /// Optional minimum power rating in watts.
    pub power_rating_min_w: Option<f64>,
}

/// Per-project rule configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LintProfile {
    /// Profile schema version.
    pub schema_version: u32,
    /// Standard or strict metadata enforcement.
    #[serde(default)]
    pub mode: ProfileMode,
    /// Component reference to intent metadata.
    #[serde(default)]
    pub components: BTreeMap<String, ComponentIntent>,
    /// References intentionally exempt from strict metadata requirements.
    #[serde(default)]
    pub ignore_components: BTreeSet<String>,
}

impl LintProfile {
    /// Load a JSON profile from disk.
    ///
    /// # Errors
    /// Returns an I/O or JSON decoding error.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, MetadataError> {
        let content = fs::read_to_string(path).map_err(MetadataError::Io)?;
        let profile: Self = serde_json::from_str(&content).map_err(MetadataError::Json)?;
        if profile.schema_version != 1 {
            return Err(MetadataError::UnsupportedVersion(profile.schema_version));
        }
        Ok(profile)
    }

    /// Validate declared intent against placed components in the schematic.
    #[must_use]
    pub fn validate(&self, document: &SchematicDocument) -> Vec<Diagnostic> {
        let facts = SchematicFacts::from_document(document);
        let mut diagnostics = Vec::new();
        let references: BTreeSet<_> = facts.components.iter().map(|item| item.reference.clone()).collect();

        for reference in self.components.keys() {
            if !references.contains(reference) {
                diagnostics.push(Diagnostic::new("SCH021", Severity::Error,
                    format!("profile metadata references missing component {reference}")));
            }
        }

        for component in &facts.components {
            let Some(intent) = self.components.get(&component.reference) else {
                if self.mode == ProfileMode::Strict && !self.ignore_components.contains(&component.reference) {
                    diagnostics.push(Diagnostic::new("SCH021", Severity::Error,
                        format!("{} has no component-intent metadata", component.reference)));
                }
                continue;
            };

            if intent.function.as_deref().is_none_or(str::is_empty)
                || intent.rationale.as_deref().is_none_or(str::is_empty)
            {
                diagnostics.push(Diagnostic::new("SCH020", Severity::Error,
                    format!("{} must document both its circuit function and design rationale", component.reference)));
            }

            if intent.kind == ComponentKind::Capacitor && intent.role.0 == "decoupling" {
                match &intent.decoupling {
                    Some(contract) => {
                        if !valid_pin_ref(&contract.target_pin) {
                            diagnostics.push(Diagnostic::new("SCH022", Severity::Error,
                                format!("{} has invalid decoupling target pin {:?}", component.reference, contract.target_pin)));
                        }
                        if contract.target_terminal.is_empty() || contract.return_terminal.is_empty()
                            || contract.return_net.is_empty() || contract.cluster.is_empty()
                        {
                            diagnostics.push(Diagnostic::new("SCH022", Severity::Error,
                                format!("{} decoupling metadata must define target/return terminals, return net, and cluster", component.reference)));
                        }
                        if contract.target_terminal == contract.return_terminal {
                            diagnostics.push(Diagnostic::new("SCH022", Severity::Error,
                                format!("{} maps both decoupling terminals to the same capacitor terminal", component.reference)));
                        }
                    }
                    None => diagnostics.push(Diagnostic::new("SCH022", Severity::Error,
                        format!("{} is a decoupling capacitor but does not identify its target pin and terminal/net contract", component.reference))),
                }
            }

            if intent.kind == ComponentKind::Capacitor
                && !matches!(intent.role.0.as_str(), "decoupling" | "bulk" | "filter" | "timing" | "coupling" | "snubber" | "reservoir" | "safety")
            {
                diagnostics.push(Diagnostic::new("SCH021", Severity::Warning,
                    format!("{} uses unknown capacitor role {:?}; document its meaning or use a standard role", component.reference, intent.role.0)));
            }
        }

        let mut by_target: BTreeMap<&str, Vec<(&str, &DecouplingIntent)>> = BTreeMap::new();
        for (reference, intent) in &self.components {
            if intent.kind == ComponentKind::Capacitor && intent.role.0 == "decoupling" {
                if let Some(contract) = &intent.decoupling {
                    by_target.entry(&contract.target_pin).or_default().push((reference, contract));
                }
            }
        }
        for (target, capacitors) in by_target {
            let returns: BTreeSet<_> = capacitors.iter().map(|(_, item)| item.return_net.as_str()).collect();
            let clusters: BTreeSet<_> = capacitors.iter().map(|(_, item)| item.cluster.as_str()).collect();
            if returns.len() > 1 {
                diagnostics.push(Diagnostic::new("SCH022", Severity::Error,
                    format!("decouplers for {target} disagree on their return net: {}", returns.into_iter().collect::<Vec<_>>().join(", "))));
            }
            if clusters.len() > 1 {
                diagnostics.push(Diagnostic::new("SCH022", Severity::Warning,
                    format!("decouplers for {target} are split across cluster identifiers: {}", clusters.into_iter().collect::<Vec<_>>().join(", "))));
            }
        }
        diagnostics
    }
}

fn valid_pin_ref(value: &str) -> bool {
    let Some((reference, pin)) = value.rsplit_once('.') else { return false; };
    !reference.is_empty() && !pin.is_empty() && pin.chars().all(|character| character.is_ascii_alphanumeric())
}

/// Profile loading and validation errors.
#[derive(Debug, Error)]
pub enum MetadataError {
    /// Profile could not be read.
    #[error("could not read lint profile: {0}")]
    Io(#[from] std::io::Error),
    /// Profile is not valid JSON.
    #[error("invalid lint profile JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Unsupported profile schema.
    #[error("unsupported lint profile schema version {0}; expected 1")]
    UnsupportedVersion(u32),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{ComponentKind, ComponentRole, LintProfile, ProfileMode};

    #[test]
    fn decoupling_intent_preserves_pin_and_terminal_mapping() {
        let json = r#"{"schema_version":1,"mode":"strict","components":{"C1":{"kind":"capacitor","role":"decoupling","function":"Local bypass","rationale":"Minimize loop area","decoupling":{"target_pin":"U1.21","target_terminal":"1","return_terminal":"2","return_net":"GND","cluster":"U1.VREG_1V2"}}}}"#;
        let profile: LintProfile = serde_json::from_str(json).expect("valid fixture");
        let intent = profile.components.get("C1").expect("C1 metadata");
        assert_eq!(intent.kind, ComponentKind::Capacitor);
        assert_eq!(intent.role, ComponentRole("decoupling".to_owned()));
        assert_eq!(intent.decoupling.as_ref().map(|item| item.target_pin.as_str()), Some("U1.21"));
        assert_eq!(profile.mode, ProfileMode::Strict);
    }
}
