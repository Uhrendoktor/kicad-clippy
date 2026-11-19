use crate::rule_id::BuiltinRuleId;
use std::collections::{BTreeMap, btree_map::Entry};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Effective level for a configurable custom rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleLevel {
    /// Disable the rule.
    Allow,
    /// Emit a warning.
    Warn,
    /// Emit an error.
    Deny,
}

/// Stable metadata used to document and configure a rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuleDescriptor {
    /// Stable identifier, for example SCH001.
    pub id: String,
    /// Short display name.
    pub name: String,
    /// Rule category such as electrical, layout, or documentation.
    pub category: String,
    /// Concise statement of what the rule checks.
    pub summary: String,
    /// Longer explanation of the design reason for the rule.
    pub explanation: String,
    /// Default level when no configuration overrides it.
    pub default_level: RuleLevel,
    /// Optional documentation URL.
    pub documentation_url: Option<String>,
    /// Searchable tags.
    pub tags: Vec<String>,
}

impl RuleDescriptor {
    /// Creates a rule descriptor from the supplied ID, display text, explanation, and default severity.
    ///
    /// # Arguments
    ///
    /// * `id` — Stable rule identifier used as the registry lookup key.
    /// * `name` — Short display name shown to users when the rule is listed.
    /// * `category` — Rule family used to group related checks, such as electrical, layout, or documentation.
    /// * `summary` — Concise statement of the condition that the rule checks.
    /// * `explanation` — Longer rationale describing why the check exists and what design intent it protects.
    /// * `default_level` — Severity level applied when no user configuration overrides this rule.
    ///
    /// # Returns
    ///
    /// A `RuleDescriptor` populated with the supplied metadata and with no documentation URL or tags.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        category: impl Into<String>,
        summary: impl Into<String>,
        explanation: impl Into<String>,
        default_level: RuleLevel,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            category: category.into(),
            summary: summary.into(),
            explanation: explanation.into(),
            default_level,
            documentation_url: None,
            tags: Vec::new(),
        }
    }
}

/// Builds the complete descriptor catalogue for all stable built-in schematic rules.
///
/// The descriptor catalogue is metadata only: it does not enable, disable, or
/// change the severity of a rule. Rule execution and diagnostic severity remain
/// owned by the rules crate until a project rule-policy layer is implemented.
///
/// # Returns
///
/// A deterministic registry containing exactly one descriptor for each ID in
/// `BuiltinRuleId::ALL`.
///
/// # Errors
///
/// Returns `RuleRegistryError` if the built-in descriptor definitions contain a duplicate ID.
pub fn builtin_rule_registry() -> Result<RuleRegistry, RuleRegistryError> {
    let mut registry = RuleRegistry::new();
    for id in BuiltinRuleId::ALL {
        registry.register(builtin_rule_descriptor(id))?;
    }
    Ok(registry)
}

/// Creates the canonical descriptive metadata for one built-in schematic rule.
///
/// # Arguments
///
/// * `id` — Stable built-in rule identifier whose descriptive metadata is requested.
///
/// # Returns
///
/// The canonical descriptor, including its stable code, category, summary,
/// rationale, default policy level, documentation link, and search tags.
fn builtin_rule_descriptor(id: BuiltinRuleId) -> RuleDescriptor {
    let (name, category, summary, explanation, level, tags): (
        &str,
        &str,
        &str,
        &str,
        RuleLevel,
        &[&str],
    ) = match id {
        BuiltinRuleId::Sch001 => (
            "wire geometry",
            "layout",
            "Checks wire endpoints, orthogonal routing, zero-length segments, overlaps, crossings, and unmarked T-junctions.",
            "Consistent wire geometry makes schematic connectivity visible and reviewable. Explicit junction semantics are preserved; native ERC remains authoritative for electrical connectivity.",
            RuleLevel::Deny,
            &["wire", "geometry", "junction"],
        ),
        BuiltinRuleId::Sch002 => (
            "label topology",
            "connectivity",
            "Checks label identity and reuse across physical wire islands, including hierarchical twig termination.",
            "Labels affect logical connectivity, but they do not create physical wire edges. This rule identifies ambiguous or inconsistent label use without replacing native ERC.",
            RuleLevel::Deny,
            &["label", "topology", "connectivity"],
        ),
        BuiltinRuleId::Sch003 => (
            "label and wire clearance",
            "annotation",
            "Detects estimated label glyph bounds that overlap wire interiors.",
            "Visible label text should remain legible and should not be obscured by wire strokes. Text bounds are estimates based on supported KiCad stroke-font metrics.",
            RuleLevel::Deny,
            &["label", "text", "clearance"],
        ),
        BuiltinRuleId::Sch004 => (
            "physical pin connectivity",
            "connectivity",
            "Validates profile-declared direct-wire, common-pin, physical-wire-group, and separate-wire contracts.",
            "Project profiles can declare physical connections that must be visibly wired. The rule uses transformed pin anchors and wire-only topology; labels never substitute for physical wire continuity.",
            RuleLevel::Deny,
            &["profile", "pins", "physical-connectivity"],
        ),
        BuiltinRuleId::Sch005 => (
            "pin orientation",
            "layout",
            "Checks that connected wire axes align with embedded symbol pin axes.",
            "Aligned pin stubs make connection intent easier to inspect. Unsupported non-cardinal axis cases are skipped rather than assigned a guessed orientation.",
            RuleLevel::Deny,
            &["pins", "orientation", "wire"],
        ),
        BuiltinRuleId::Sch006 => (
            "component spacing",
            "layout",
            "Enforces profile-declared minimum spacing between placed functional-group member origins.",
            "A declared spacing contract keeps related components consistently arranged. The current check measures symbol origins, not full body-to-body clearance.",
            RuleLevel::Deny,
            &["groups", "spacing", "profile"],
        ),
        BuiltinRuleId::Sch007 => (
            "native group membership",
            "design-structure",
            "Checks profile-declared member references against a native KiCad schematic group.",
            "Binding profile groups to native group UUIDs catches drift between documented functional grouping and actual group membership.",
            RuleLevel::Deny,
            &["groups", "metadata", "kicad"],
        ),
        BuiltinRuleId::Sch008 => (
            "annotation text-box geometry",
            "annotation",
            "Checks required text-box geometry, font, margins, sizing, overlap, and wire penetration.",
            "Structured annotation boxes should have usable dimensions and remain readable without colliding with other content or electrical graphics.",
            RuleLevel::Deny,
            &["annotation", "text-box", "geometry"],
        ),
        BuiltinRuleId::Sch009 => (
            "group containment and frame layout",
            "design-structure",
            "Checks profile-declared group frames, member geometry, associated annotations, and overlap between independently declared frame interiors.",
            "Functional-group frames should contain their declared contents and should not overlap other independently declared frames. Only explicitly referenced frame UUIDs are treated as group boundaries; shared edges are allowed.",
            RuleLevel::Deny,
            &["groups", "frames", "containment", "profile"],
        ),
        BuiltinRuleId::Sch010 => (
            "symbol field clearance",
            "annotation",
            "Checks estimated visible symbol-field bounds against fields, symbol bodies, wires, pins, labels, and annotation boxes.",
            "Reference and Value fields should remain readable and avoid collisions with nearby schematic content. Text bounds are estimates; exact renderer geometry and maximum field-to-body gaps remain future work.",
            RuleLevel::Deny,
            &["fields", "text", "clearance"],
        ),
        BuiltinRuleId::Sch011 => (
            "block annotation structure",
            "documentation",
            "Requires multiline annotation text boxes to have a visible border.",
            "A visible border distinguishes multi-line explanatory blocks from free-standing notes and improves consistent schematic documentation.",
            RuleLevel::Deny,
            &["annotation", "documentation", "text-box"],
        ),
        BuiltinRuleId::Sch012 => (
            "frame content clearance",
            "design-structure",
            "Enforces a profile-declared minimum inset between functional-group frame edges and contained schematic content.",
            "An explicit inset prevents group members and associated graphics from crowding their frame boundary. Coverage remains limited to the documented supported geometry families.",
            RuleLevel::Deny,
            &["groups", "frames", "clearance", "profile"],
        ),
        BuiltinRuleId::Sch013 => (
            "wire obstacle clearance",
            "layout",
            "Checks wire strokes against supported symbol, annotation, sheet, bus, table, image, and vector-graphic obstacles.",
            "Wire-to-graphic collisions reduce readability and can obscure intended topology. Geometry uses resolved transforms and widths where authoritative facts are available; unresolved cases remain conservative.",
            RuleLevel::Deny,
            &["wire", "obstacles", "geometry", "clearance"],
        ),
        BuiltinRuleId::Sch014 => (
            "shared pin and net contracts",
            "electrical-intent",
            "Validates profile-declared component pin and connector pin mappings against the exported KiCad netlist.",
            "A profile can assert the logical net expected at a particular component pin. The exported netlist is the authority for these relationships, not label-string comparison.",
            RuleLevel::Deny,
            &["profile", "pins", "netlist", "connectors"],
        ),
        BuiltinRuleId::Sch015 => (
            "required no-connect markers",
            "electrical-intent",
            "Checks explicit no-connect marker placement and profile-declared pins that require a marker.",
            "An explicit no-connect flag records intentional omission at a placed pin. Unresolved pin geometry is reported as unverified; native ERC remains responsible for electrical unconnected-pin semantics.",
            RuleLevel::Deny,
            &["pins", "no-connect", "profile"],
        ),
        BuiltinRuleId::Sch016 => (
            "wire compactness",
            "layout",
            "Compares the total length of wire segments wholly inside a declared group frame with its positive wire-length budget.",
            "A profile-declared budget can discourage unnecessarily long internal wiring while keeping the threshold project-specific. Over-budget findings are warnings.",
            RuleLevel::Warn,
            &["groups", "wire", "length", "profile"],
        ),
        BuiltinRuleId::Sch017 => (
            "circuit documentation",
            "documentation",
            "Validates functional-group membership, description references, and declared documentation evidence.",
            "Documented group intent should resolve to actual schematic members and a rendered description with traceable design evidence.",
            RuleLevel::Deny,
            &["groups", "documentation", "evidence"],
        ),
        BuiltinRuleId::Sch018 => (
            "label text collision",
            "annotation",
            "Detects estimated label glyph bounds that overlap other labels, visible symbol fields, or annotation boxes.",
            "Label collisions make net names difficult to read and can conceal nearby component annotations. Glyph bounds are approximations of the rendered stroke font.",
            RuleLevel::Deny,
            &["labels", "text", "collision"],
        ),
        BuiltinRuleId::Sch019 => (
            "label-kind consistency",
            "connectivity",
            "Detects reuse of one label text with inconsistent local, global, or hierarchical label kinds.",
            "Repeated label strings with different scope semantics can produce surprising logical connectivity and should be made explicit.",
            RuleLevel::Deny,
            &["labels", "scope", "connectivity"],
        ),
        BuiltinRuleId::Sch020 => (
            "component function and rationale",
            "documentation",
            "Requires metadata-declared components to provide function and rationale text.",
            "Component intent should explain both what a part does and why the chosen implementation or topology is appropriate.",
            RuleLevel::Deny,
            &["components", "metadata", "documentation"],
        ),
        BuiltinRuleId::Sch021 => (
            "component metadata and role contracts",
            "design-intent",
            "Checks strict component metadata coverage, stale profile references, and supported role-specific connection and connector declarations.",
            "Component metadata should identify every required placed part and provide the electrical nodes required by supported roles. Connector declarations additionally require an interface standard and at least one pin mapping; unsupported roles are not inferred.",
            RuleLevel::Deny,
            &["components", "metadata", "roles", "profile"],
        ),
        BuiltinRuleId::Sch022 => (
            "decoupling contracts",
            "electrical-intent",
            "Checks decoupling target pins, distinct capacitor terminals, expected return nets, netlist membership, and cluster consistency.",
            "A decoupling declaration must identify the target IC pin, the capacitor's target and return terminals, the expected return net, and a cluster. When a netlist is supplied, target-side and return-side memberships are verified against KiCad's exported connectivity.",
            RuleLevel::Deny,
            &["decoupling", "capacitors", "netlist", "profile"],
        ),
    };

    let mut descriptor =
        RuleDescriptor::new(id.as_str(), name, category, summary, explanation, level);
    descriptor.documentation_url =
        Some("https://github.com/Uhrendoktor/kicad-clippy/blob/main/docs/rules.md".to_owned());
    descriptor.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
    descriptor
}

/// Error raised when a rule identifier is registered more than once.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("rule identifier {0:?} is already registered")]
pub struct RuleRegistryError(
    /// Stable identifier of the descriptor rejected because that ID was already registered.
    pub String,
);

/// Deterministic catalogue of rule metadata.
#[derive(Clone, Debug, Default)]
pub struct RuleRegistry {
    /// Rule descriptors keyed by stable ID; the ordered map keeps catalogue iteration deterministic.
    rules: BTreeMap<String, RuleDescriptor>,
}

impl RuleRegistry {
    /// Creates an empty deterministic catalogue for rule descriptors.
    ///
    /// # Returns
    ///
    /// A registry with no descriptors registered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a descriptor under its stable ID. Existing IDs are rejected so one rule cannot silently replace another.
    ///
    /// # Arguments
    ///
    /// * `descriptor` — Complete rule metadata record to insert into the catalogue.
    ///
    /// # Returns
    ///
    /// `Ok(())` after insertion, or `RuleRegistryError` when the ID is already registered.
    ///
    /// # Errors
    ///
    /// Returns `RuleRegistryError` when another descriptor already uses the same identifier.
    pub fn register(&mut self, descriptor: RuleDescriptor) -> Result<(), RuleRegistryError> {
        match self.rules.entry(descriptor.id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(descriptor);
                Ok(())
            }
            Entry::Occupied(entry) => Err(RuleRegistryError(entry.key().clone())),
        }
    }

    /// Looks up a rule descriptor by its stable ID without changing the catalogue.
    ///
    /// # Arguments
    ///
    /// * `id` — Stable rule identifier used as the registry lookup key.
    ///
    /// # Returns
    ///
    /// A borrowed descriptor when `id` exists, or `None` when it is not registered.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&RuleDescriptor> {
        self.rules.get(id)
    }

    /// Returns descriptors in lexicographic identifier order, as provided by the underlying `BTreeMap`.
    ///
    /// # Returns
    ///
    /// An iterator over all registered descriptors in stable ID order.
    pub fn iter(&self) -> impl Iterator<Item = &RuleDescriptor> {
        self.rules.values()
    }

    /// Returns the number of distinct rule descriptors currently registered.
    ///
    /// # Returns
    ///
    /// The current number of registered descriptors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Reports whether the registry contains any rule descriptors.
    ///
    /// # Returns
    ///
    /// `true` when no descriptors have been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}
#[cfg(test)]
/// Unit tests for rule descriptor construction, lookup, ordering, and duplicate rejection.
mod tests {
    use super::{RuleDescriptor, RuleLevel, RuleRegistry};
    /// Builds a representative SCH001 descriptor used by the registry unit tests.
    ///
    /// # Returns
    ///
    /// A fixed SCH001 descriptor used as test data.
    fn sample_rule() -> RuleDescriptor {
        RuleDescriptor::new(
            "SCH001",
            "wire geometry",
            "layout",
            "Keep wires orthogonal.",
            "Orthogonal routing is easier to inspect and maintain.",
            RuleLevel::Deny,
        )
    }
    /// Checks that registry iteration is ordered by rule ID and duplicate identifiers are rejected.
    #[test]
    fn registry_is_deterministic_and_rejects_duplicate_ids() {
        let mut registry = RuleRegistry::new();

        assert!(registry.register(sample_rule()).is_ok());
        assert_eq!(
            registry.get("SCH001").map(|rule| rule.name.as_str()),
            Some("wire geometry")
        );
        assert!(registry.register(sample_rule()).is_err());
        assert_eq!(registry.len(), 1);
    }

    /// Checks that the built-in descriptor catalogue covers every stable rule ID exactly once.
    #[test]
    fn builtin_registry_covers_every_typed_rule_id() -> Result<(), super::RuleRegistryError> {
        use crate::rule_id::BuiltinRuleId;

        let registry = super::builtin_rule_registry()?;
        assert_eq!(registry.len(), BuiltinRuleId::ALL.len());
        for id in BuiltinRuleId::ALL {
            let descriptor = registry
                .get(id.as_str())
                .expect("every built-in rule must have a descriptor");
            assert_eq!(descriptor.id, id.as_str());
            assert!(!descriptor.name.trim().is_empty());
            assert!(!descriptor.category.trim().is_empty());
            assert!(!descriptor.summary.trim().is_empty());
            assert!(!descriptor.explanation.trim().is_empty());
            assert!(descriptor.documentation_url.is_some());
            assert!(!descriptor.tags.is_empty());
        }
        Ok(())
    }

    /// Checks that the latest profile-rule descriptors summarize their implemented contracts.
    #[test]
    fn sch021_and_sch022_descriptors_match_the_profile_contracts()
    -> Result<(), super::RuleRegistryError> {
        let registry = super::builtin_rule_registry()?;
        let sch021 = registry.get("SCH021").expect("SCH021 descriptor exists");
        assert!(sch021.summary.contains("role-specific"));
        assert!(sch021.summary.contains("connector"));
        let sch022 = registry.get("SCH022").expect("SCH022 descriptor exists");
        assert!(sch022.summary.contains("decoupling"));
        assert!(sch022.summary.contains("return nets"));
        Ok(())
    }
}
