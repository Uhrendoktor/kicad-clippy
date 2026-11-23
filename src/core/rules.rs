use crate::core::{
    diagnostic::{Diagnostic, Severity},
    rule_id::BuiltinRuleId,
};
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

/// Named baseline profile applied to built-in rule descriptors when a policy file is supplied.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePreset {
    /// Use each rule descriptor's documented default level.
    #[default]
    Default,
    /// Treat every built-in rule finding as an error unless explicitly overridden.
    Strict,
    /// Treat every built-in rule finding as a warning unless explicitly overridden.
    Advisory,
}

/// Optional project policy that overrides the effective level of built-in schematic rules.
///
/// Supplying a policy file selects a named baseline (`default`, `strict`, or
/// `advisory`) for every built-in rule. Entries in `rules` override that
/// baseline. Omitting a policy file leaves current rule-emitted severities unchanged.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RulePolicy {
    /// Named baseline applied before per-rule overrides.
    #[serde(default)]
    pub preset: RulePreset,
    /// Explicit per-rule overrides keyed by stable built-in ID, such as `SCH013`.
    #[serde(default)]
    pub rules: BTreeMap<String, RuleLevel>,
}

impl RulePolicy {
    /// Validates that every configured key names a rule in the supplied catalogue.
    ///
    /// # Arguments
    ///
    /// * `registry` — Complete rule catalogue used to validate stable policy keys.
    ///
    /// # Returns
    ///
    /// Ok when every configured rule ID exists in `registry`.
    ///
    /// # Errors
    ///
    /// Returns `RulePolicyError` when a configured ID is unknown.
    pub fn validate(&self, registry: &RuleRegistry) -> Result<(), RulePolicyError> {
        for id in self.rules.keys() {
            if registry.get(id).is_none() {
                return Err(RulePolicyError(id.clone()));
            }
        }
        Ok(())
    }

    /// Applies the selected named preset and explicit overrides to built-in diagnostics.
    ///
    /// External diagnostics, including native `KiCad` ERC findings, are retained
    /// without severity changes. An `allow` policy removes the matching built-in
    /// findings; `warn` and `deny` set their effective severities.
    ///
    /// # Arguments
    ///
    /// * `registry` — Complete built-in descriptor catalogue supplying default levels.
    /// * `diagnostics` — Normalized native and custom findings produced by the lint invocation.
    ///
    /// # Returns
    ///
    /// Diagnostics after built-in policy levels have been applied.
    ///
    /// # Errors
    ///
    /// Returns `RulePolicyError` when an explicit policy key does not exist in the catalogue.
    pub fn apply(
        &self,
        registry: &RuleRegistry,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<Vec<Diagnostic>, RulePolicyError> {
        self.validate(registry)?;
        let mut effective = Vec::with_capacity(diagnostics.len());
        for mut diagnostic in diagnostics {
            let Some(descriptor) = registry.get(&diagnostic.code) else {
                effective.push(diagnostic);
                continue;
            };
            let preset_level = match self.preset {
                RulePreset::Default => descriptor.default_level,
                RulePreset::Strict => RuleLevel::Deny,
                RulePreset::Advisory => RuleLevel::Warn,
            };
            let level = self
                .rules
                .get(&diagnostic.code)
                .copied()
                .unwrap_or(preset_level);
            match level {
                RuleLevel::Allow => continue,
                RuleLevel::Warn => diagnostic.severity = Severity::Warning,
                RuleLevel::Deny => diagnostic.severity = Severity::Error,
            }
            effective.push(diagnostic);
        }
        Ok(effective)
    }
}

/// Error raised when a rule policy references an unknown built-in rule identifier.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("rule policy references unknown built-in rule {0:?}")]
pub struct RulePolicyError(
    /// Unknown rule ID supplied in the project policy.
    pub String,
);

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

/// Static descriptive data for one built-in rule.
struct BuiltinRuleMetadata {
    /// Stable enum identifier owning this descriptor.
    id: BuiltinRuleId,
    /// Concise display name used by rule-listing interfaces.
    name: &'static str,
    /// High-level family used to group related rules.
    category: &'static str,
    /// One-sentence statement of the enforced condition.
    summary: &'static str,
    /// Rationale and semantic limits of the check.
    explanation: &'static str,
    /// Descriptive default policy level; not applied until policy integration exists.
    level: RuleLevel,
    /// Search terms used by rule discovery and filtering.
    tags: &'static [&'static str],
}

/// Canonical metadata for the complete stable built-in rule set.
const BUILTIN_RULE_METADATA: [BuiltinRuleMetadata; 31] = [
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch001,
        name: "wire geometry",
        category: "layout",
        summary: "Checks wire endpoints, orthogonal routing, zero-length segments, overlaps, crossings, and unmarked T-junctions.",
        explanation: "Consistent wire geometry makes schematic connectivity visible and reviewable. Explicit junction semantics are preserved; native ERC remains authoritative for electrical connectivity.",
        level: RuleLevel::Deny,
        tags: &["wire", "geometry", "junction"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch002,
        name: "label topology",
        category: "connectivity",
        summary: "Checks label identity and reuse across physical wire islands, including hierarchical twig termination.",
        explanation: "Labels affect logical connectivity, but they do not create physical wire edges. This rule identifies ambiguous or inconsistent label use without replacing native ERC.",
        level: RuleLevel::Deny,
        tags: &["label", "topology", "connectivity"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch003,
        name: "label and wire clearance",
        category: "annotation",
        summary: "Detects estimated label glyph bounds that overlap wire interiors.",
        explanation: "Visible label text should remain legible and should not be obscured by wire strokes. Text bounds are estimates based on supported KiCad stroke-font metrics.",
        level: RuleLevel::Deny,
        tags: &["label", "text", "clearance"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch004,
        name: "physical pin connectivity",
        category: "connectivity",
        summary: "Validates profile-declared direct-wire, common-pin, physical-wire-group, and separate-wire contracts.",
        explanation: "Project profiles can declare physical connections that must be visibly wired. The rule uses transformed pin anchors and wire-only topology; labels never substitute for physical wire continuity.",
        level: RuleLevel::Deny,
        tags: &["profile", "pins", "physical-connectivity"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch005,
        name: "pin orientation",
        category: "layout",
        summary: "Checks connected wire segments against the full transformed embedded pin axis, including non-cardinal rotations.",
        explanation: "Aligned pin stubs make connection intent easier to inspect. The placed symbol's finite rotation and mirror transform are applied to the pin axis before comparing the attached wire direction; unresolved transforms remain unverified by omission.",
        level: RuleLevel::Deny,
        tags: &["pins", "orientation", "wire"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch006,
        name: "component body spacing and alignment",
        category: "layout",
        summary: "Enforces profile-declared origin spacing, curve-aware transformed body clearance and compactness, and horizontal-row or vertical-column member alignment.",
        explanation: "Functional-group placement contracts can constrain origin locations, minimum body separation, and maximum pairwise body gaps. Circular arcs and cubic Béziers use conservative flattened paths; genuinely incomplete geometry remains unverified.",
        level: RuleLevel::Deny,
        tags: &["groups", "spacing", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch007,
        name: "native group membership",
        category: "design-structure",
        summary: "Checks profile-declared member references against a native KiCad schematic group.",
        explanation: "Binding profile groups to native group UUIDs catches drift between documented functional grouping and actual group membership.",
        level: RuleLevel::Deny,
        tags: &["groups", "metadata", "kicad"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch008,
        name: "annotation text-box geometry",
        category: "annotation",
        summary: "Checks required text-box geometry, font, margins, sizing, overlap, and wire penetration.",
        explanation: "Structured annotation boxes should have usable dimensions and remain readable without colliding with other content or electrical graphics.",
        level: RuleLevel::Deny,
        tags: &["annotation", "text-box", "geometry"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch009,
        name: "group containment and frame layout",
        category: "design-structure",
        summary: "Checks profile-declared group frames, member geometry, associated annotations, and overlap between independently declared frame interiors.",
        explanation: "Functional-group frames should contain their declared contents and should not overlap other independently declared frames. Only explicitly referenced frame UUIDs are treated as group boundaries; shared edges are allowed.",
        level: RuleLevel::Deny,
        tags: &["groups", "frames", "containment", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch010,
        name: "symbol field clearance",
        category: "annotation",
        summary: "Checks estimated visible symbol-field bounds against schematic objects and optional per-component maximum gaps to the selected-unit symbol body.",
        explanation: "Reference and Value fields should remain readable and avoid collisions with nearby content. Profile maximum field-to-body gaps use estimated glyph rectangles and supported transformed body geometry; exact renderer bounds remain approximate.",
        level: RuleLevel::Deny,
        tags: &["fields", "text", "clearance"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch011,
        name: "block annotation structure",
        category: "documentation",
        summary: "Requires multiline annotation text boxes to have a visible border.",
        explanation: "A visible border distinguishes multi-line explanatory blocks from free-standing notes and improves consistent schematic documentation.",
        level: RuleLevel::Deny,
        tags: &["annotation", "documentation", "text-box"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch012,
        name: "frame content clearance",
        category: "design-structure",
        summary: "Enforces a profile-declared minimum inset between functional-group frame edges and contained schematic content.",
        explanation: "An explicit inset prevents group members and associated graphics from crowding their frame boundary. Coverage remains limited to the documented supported geometry families.",
        level: RuleLevel::Deny,
        tags: &["groups", "frames", "clearance", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch013,
        name: "wire obstacle clearance",
        category: "layout",
        summary: "Checks wire strokes against supported symbol, annotation, sheet, bus, table, image, and vector-graphic obstacles.",
        explanation: "Wire-to-graphic collisions reduce readability and can obscure intended topology. Geometry uses resolved transforms and widths where authoritative facts are available; unresolved cases remain conservative.",
        level: RuleLevel::Deny,
        tags: &["wire", "obstacles", "geometry", "clearance"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch014,
        name: "shared pin and net contracts",
        category: "electrical-intent",
        summary: "Validates profile-declared component pin and connector pin mappings against the exported KiCad netlist.",
        explanation: "A profile can assert the logical net expected at a particular component pin. The exported netlist is the authority for these relationships, not label-string comparison.",
        level: RuleLevel::Deny,
        tags: &["profile", "pins", "netlist", "connectors"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch015,
        name: "required no-connect markers",
        category: "electrical-intent",
        summary: "Checks explicit no-connect marker placement and profile-declared pins that require a marker.",
        explanation: "An explicit no-connect flag records intentional omission at a placed pin. Unresolved pin geometry is reported as unverified; native ERC remains responsible for electrical unconnected-pin semantics.",
        level: RuleLevel::Deny,
        tags: &["pins", "no-connect", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch016,
        name: "wire compactness",
        category: "layout",
        summary: "Compares the total length of wire segments wholly inside a declared group frame with its positive wire-length budget.",
        explanation: "A profile-declared budget can discourage unnecessarily long internal wiring while keeping the threshold project-specific. Over-budget findings are warnings.",
        level: RuleLevel::Warn,
        tags: &["groups", "wire", "length", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch017,
        name: "circuit documentation",
        category: "documentation",
        summary: "Validates functional-group membership, description references, and declared documentation evidence.",
        explanation: "Documented group intent should resolve to actual schematic members and a rendered description with traceable design evidence.",
        level: RuleLevel::Deny,
        tags: &["groups", "documentation", "evidence"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch018,
        name: "label text collision",
        category: "annotation",
        summary: "Detects estimated label glyph bounds that overlap other labels, visible symbol fields, or annotation boxes.",
        explanation: "Label collisions make net names difficult to read and can conceal nearby component annotations. Glyph bounds are approximations of the rendered stroke font.",
        level: RuleLevel::Deny,
        tags: &["labels", "text", "collision"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch019,
        name: "label-kind consistency",
        category: "connectivity",
        summary: "Detects reuse of one label text with inconsistent local, global, or hierarchical label kinds.",
        explanation: "Repeated label strings with different scope semantics can produce surprising logical connectivity and should be made explicit.",
        level: RuleLevel::Deny,
        tags: &["labels", "scope", "connectivity"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch020,
        name: "component function and rationale",
        category: "documentation",
        summary: "Requires metadata-declared components to provide function and rationale text.",
        explanation: "Component intent should explain both what a part does and why the chosen implementation or topology is appropriate.",
        level: RuleLevel::Deny,
        tags: &["components", "metadata", "documentation"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch021,
        name: "component metadata and role contracts",
        category: "design-intent",
        summary: "Checks strict component metadata coverage, stale profile references, and supported role-specific connection and connector declarations.",
        explanation: "Component metadata should identify every required placed part and provide the electrical nodes required by supported roles. Connector declarations additionally require an interface standard and at least one pin mapping; unsupported roles are not inferred.",
        level: RuleLevel::Deny,
        tags: &["components", "metadata", "roles", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch022,
        name: "decoupling contracts",
        category: "electrical-intent",
        summary: "Checks decoupling target pins, distinct capacitor terminals, expected return nets, netlist membership, and cluster consistency.",
        explanation: "A decoupling declaration must identify the target IC pin, the capacitor's target and return terminals, the expected return net, and a cluster. When a netlist is supplied, target-side and return-side memberships are verified against KiCad's exported connectivity.",
        level: RuleLevel::Deny,
        tags: &["decoupling", "capacitors", "netlist", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch023,
        name: "required title-block fields",
        category: "documentation",
        summary: "Checks project-declared title, date, revision, company and comment fields on the current sheet.",
        explanation: "A project can require selected native title-block fields without forcing a universal title-block style. Only fields explicitly listed by the profile are enforced.",
        level: RuleLevel::Deny,
        tags: &["title-block", "metadata", "sheet", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch024,
        name: "document UUID uniqueness",
        category: "document-integrity",
        summary: "Detects UUID reuse by multiple objects in a root schematic and its referenced child-sheet hierarchy.",
        explanation: "Duplicate object identifiers make selection, grouping and cross-reference behavior ambiguous. Path-based root schematics are traversed through each distinct referenced Sheetfile, with canonical-path cycle protection. Missing or invalid child sheets produce warnings so incomplete hierarchy coverage is explicit; in-memory documents retain single-document scope.",
        level: RuleLevel::Deny,
        tags: &["uuid", "document-integrity", "hierarchy"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Pcb001,
        name: "decoupling pad proximity",
        category: "pcb-design-intent",
        summary: "Checks profile-declared maximum pad-center distance from a target IC supply pad to its decoupling capacitor pad and verifies the declared capacitor return pad net.",
        explanation: "PCB001 measures actual placed pad anchors after applying the serialized footprint transform, verifies target-side and return-side board net assignments, and optionally compares them with the exported schematic netlist. It does not analyze routed copper length, ground vias, zones, or return-loop quality; those require a copper connectivity model.",
        level: RuleLevel::Deny,
        tags: &["pcb", "decoupling", "pad-distance", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch025,
        name: "required test-point coverage",
        category: "testability",
        summary: "Requires profile-declared test-point component pins for each project-critical net.",
        explanation: "Critical nets should expose an intentionally declared test point. SCH025 verifies that the profile identifies a test-point component and pin for every required net, then confirms the pin-to-net assignment against KiCad's exported netlist when available.",
        level: RuleLevel::Deny,
        tags: &["test-point", "coverage", "netlist", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Pcb009,
        name: "test-point pad exposure",
        category: "pcb-testability",
        summary: "Checks that each required test-point component pin has a placed board pad on its expected net with copper and solder-mask layers.",
        explanation: "PCB009 resolves test-point reference/pad numbers against placed footprints, checks board net assignments, and verifies that the pad layer list includes copper and solder mask. This is an exposure-layer check only; it does not model probe approach, fixture geometry, enclosure access, or physical clearance around the pad.",
        level: RuleLevel::Deny,
        tags: &[
            "pcb",
            "test-point",
            "solder-mask",
            "accessibility",
            "profile",
        ],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch026,
        name: "project grid alignment",
        category: "schematic-layout",
        summary: "Checks selected schematic anchors against an opt-in profile-declared grid spacing and origin.",
        explanation: "SCH026 checks only the object families named by the profile, defaulting to placed symbol origins and label anchors. Pin anchors, junctions and wire endpoints are opt-in to avoid duplicating native ERC reports by default. Component, pin, label and coordinate exceptions are explicit; the rule does not infer or modify KiCad project grid settings.",
        level: RuleLevel::Deny,
        tags: &["grid", "alignment", "layout", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch027,
        name: "label scope policy",
        category: "connectivity-policy",
        summary: "Enforces project-declared global-label allow/deny lists and required label scopes on the loaded schematic sheet.",
        explanation: "SCH027 supports an optional global-label allow-list, a global-label deny-list, and required local/global/hierarchical scope for named labels. It inspects labels on the currently loaded sheet only; it does not resolve hierarchical parent/child sheet pins or reimplement KiCad's native label and net resolution.",
        level: RuleLevel::Deny,
        tags: &["labels", "scope", "global", "hierarchical", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch028,
        name: "functional frame title layout",
        category: "schematic-layout",
        summary: "Checks that a UUID-declared title tab is attached to its functional frame, its title box is contained by the tab, and configured frame gaps are maintained.",
        explanation: "SCH028 uses explicit frame, tab, and title-text-box UUIDs; it does not infer arbitrary rectangles as group frames. The check verifies serialized rectangular bounds and shared-edge attachment. Text-box containment is a geometric rectangle check, not an exact font-rendering or clipping simulation.",
        level: RuleLevel::Deny,
        tags: &["group-frame", "title-tab", "spacing", "layout", "profile"],
    },
    BuiltinRuleMetadata {
        id: BuiltinRuleId::Sch029,
        name: "component population metadata",
        category: "component-intent",
        summary: "Checks opt-in component population declarations against the saved base-design DNP flags and rejects placeholder Value text.",
        explanation: "SCH029 uses explicit component profile intent and KiCad's serialized DNP attribute. It validates the saved base design only; detected design-variant overrides are reported as unverified because the profile does not select an active variant.",
        level: RuleLevel::Deny,
        tags: &["population", "dnp", "bom", "component-intent", "profile"],
    },
];

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
    for metadata in BUILTIN_RULE_METADATA {
        let mut descriptor = RuleDescriptor::new(
            metadata.id.as_str(),
            metadata.name,
            metadata.category,
            metadata.summary,
            metadata.explanation,
            metadata.level,
        );
        descriptor.documentation_url =
            Some("https://github.com/Uhrendoktor/kicad-clippy/blob/main/docs/rules.md".to_owned());
        descriptor.tags = metadata.tags.iter().map(|tag| (*tag).to_owned()).collect();
        registry.register(descriptor)?;
    }
    Ok(registry)
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
    use std::collections::BTreeMap;

    use super::{RuleDescriptor, RuleLevel, RuleRegistry};
    /// Checks explicit allow, warn, and deny overrides while preserving native ERC findings.
    ///
    /// # Returns
    ///
    /// Ok when built-in severities follow the configured levels and external diagnostics remain unchanged.
    ///
    /// # Errors
    ///
    /// Returns a policy error if the test policy contains an unknown built-in rule ID.
    #[test]
    fn policy_applies_allow_warn_and_deny_without_touching_external_findings()
    -> Result<(), super::RulePolicyError> {
        use crate::core::diagnostic::{Diagnostic, Severity};

        let registry = super::builtin_rule_registry().unwrap_or_default();
        let policy = super::RulePolicy {
            preset: super::RulePreset::Default,
            rules: BTreeMap::from([
                ("SCH001".to_owned(), RuleLevel::Allow),
                ("SCH002".to_owned(), RuleLevel::Warn),
                ("SCH003".to_owned(), RuleLevel::Deny),
            ]),
        };
        let diagnostics = vec![
            Diagnostic::new("SCH001", Severity::Error, "suppressed"),
            Diagnostic::new("SCH002", Severity::Error, "warning override"),
            Diagnostic::new("SCH003", Severity::Warning, "error override"),
            Diagnostic::new("KICAD/ERC/UNDRIVEN", Severity::Note, "native note"),
        ];

        let effective = policy.apply(&registry, diagnostics)?;
        assert_eq!(effective.len(), 3);
        assert_eq!(effective[0].code, "SCH002");
        assert_eq!(effective[0].severity, Severity::Warning);
        assert_eq!(effective[1].code, "SCH003");
        assert_eq!(effective[1].severity, Severity::Error);
        assert_eq!(effective[2].code, "KICAD/ERC/UNDRIVEN");
        assert_eq!(effective[2].severity, Severity::Note);
        Ok(())
    }

    /// Checks that supplying an empty policy activates descriptor defaults for built-in rules.
    ///
    /// # Returns
    ///
    /// Ok when default-deny and default-warn descriptors set the effective severity.
    ///
    /// # Errors
    ///
    /// Returns a policy error if the descriptor defaults cannot be resolved.
    #[test]
    fn policy_uses_descriptor_defaults_for_unspecified_rules() -> Result<(), super::RulePolicyError>
    {
        use crate::core::diagnostic::{Diagnostic, Severity};

        let registry = super::builtin_rule_registry().unwrap_or_default();
        let policy = super::RulePolicy::default();
        let effective = policy.apply(
            &registry,
            vec![
                Diagnostic::new("SCH001", Severity::Warning, "default deny"),
                Diagnostic::new("SCH016", Severity::Error, "default warn"),
            ],
        )?;
        assert_eq!(effective[0].severity, Severity::Error);
        assert_eq!(effective[1].severity, Severity::Warning);
        Ok(())
    }

    /// Checks strict and advisory presets and confirms explicit entries override a preset.
    ///
    /// # Returns
    ///
    /// Ok when strict promotes all built-ins, advisory downgrades them, and explicit overrides win.
    ///
    /// # Errors
    ///
    /// Returns a policy error if the built-in rule catalogue cannot validate the test policy.
    #[test]
    fn policy_named_presets_apply_and_allow_explicit_overrides()
    -> Result<(), super::RulePolicyError> {
        use crate::core::diagnostic::{Diagnostic, Severity};

        let registry = super::builtin_rule_registry().unwrap_or_default();
        let strict = super::RulePolicy {
            preset: super::RulePreset::Strict,
            rules: BTreeMap::new(),
        };
        let strict_result = strict.apply(
            &registry,
            vec![
                Diagnostic::new("SCH001", Severity::Warning, "strict finding"),
                Diagnostic::new("SCH016", Severity::Warning, "strict warning"),
            ],
        )?;
        assert_eq!(strict_result[0].severity, Severity::Error);
        assert_eq!(strict_result[1].severity, Severity::Error);

        let advisory = super::RulePolicy {
            preset: super::RulePreset::Advisory,
            rules: BTreeMap::from([("SCH001".to_owned(), RuleLevel::Deny)]),
        };
        let advisory_result = advisory.apply(
            &registry,
            vec![
                Diagnostic::new("SCH001", Severity::Warning, "explicit deny"),
                Diagnostic::new("SCH002", Severity::Error, "advisory baseline"),
            ],
        )?;
        assert_eq!(advisory_result[0].severity, Severity::Error);
        assert_eq!(advisory_result[1].severity, Severity::Warning);
        Ok(())
    }

    /// Checks that misspelled or unsupported policy IDs are rejected before diagnostics are changed.
    ///
    /// # Returns
    ///
    /// Ok when an unknown policy key is returned as a validation error.
    ///
    /// # Errors
    ///
    /// This test returns no error because the unknown ID is the expected outcome under test.
    #[test]
    fn policy_rejects_unknown_rule_ids() {
        let registry = super::builtin_rule_registry().unwrap_or_default();
        let policy = super::RulePolicy {
            preset: super::RulePreset::Default,
            rules: BTreeMap::from([("SCH999".to_owned(), RuleLevel::Warn)]),
        };
        assert_eq!(
            policy.validate(&registry),
            Err(super::RulePolicyError("SCH999".to_owned()))
        );
    }

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
    ///
    /// # Returns
    ///
    /// Ok when every typed built-in ID has one complete descriptor.
    ///
    /// # Errors
    ///
    /// Returns a registry error if descriptor registration encounters a duplicate ID.
    #[test]
    fn builtin_registry_covers_every_typed_rule_id() -> Result<(), super::RuleRegistryError> {
        use crate::core::rule_id::BuiltinRuleId;

        let registry = super::builtin_rule_registry()?;
        assert_eq!(registry.len(), BuiltinRuleId::ALL.len());
        for id in BuiltinRuleId::ALL {
            let descriptor = registry
                .get(id.as_str())
                .ok_or_else(|| super::RuleRegistryError(id.as_str().to_owned()))?;
            assert_eq!(descriptor.id, id.as_str());
            assert_ne!(descriptor.name.trim(), "");
            assert_ne!(descriptor.category.trim(), "");
            assert_ne!(descriptor.summary.trim(), "");
            assert_ne!(descriptor.explanation.trim(), "");
            assert!(descriptor.documentation_url.is_some());
            assert_ne!(descriptor.tags.len(), 0);
        }
        Ok(())
    }

    /// Checks that the latest profile-rule descriptors summarize their implemented contracts.
    ///
    /// # Returns
    ///
    /// Ok when the SCH021 and SCH022 metadata describes their implemented contracts.
    ///
    /// # Errors
    ///
    /// Returns a registry error if descriptor registration encounters a duplicate ID.
    #[test]
    fn sch021_and_sch022_descriptors_match_the_profile_contracts()
    -> Result<(), super::RuleRegistryError> {
        let registry = super::builtin_rule_registry()?;
        let sch021 = registry
            .get("SCH021")
            .ok_or_else(|| super::RuleRegistryError("SCH021".to_owned()))?;
        assert!(sch021.summary.contains("role-specific"));
        assert!(sch021.summary.contains("connector"));
        let sch022 = registry
            .get("SCH022")
            .ok_or_else(|| super::RuleRegistryError("SCH022".to_owned()))?;
        assert!(sch022.summary.contains("decoupling"));
        assert!(sch022.summary.contains("return nets"));
        Ok(())
    }
}
