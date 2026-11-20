use std::{fmt, str::FromStr};

/// Stable identifier for a built-in schematic rule.
///
/// Built-in identifiers are represented as an enum in Rust code so call sites
/// do not need to repeat string literals. Their string representation remains
/// the public, serialized diagnostic code (for example, `SCH001`).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BuiltinRuleId {
    /// Represents the `SCH001` rule: Checks wire endpoints, orthogonal routing, zero-length segments, overlaps, crossings, and unmarked T-junctions.
    Sch001,
    /// Represents the `SCH002` rule: Checks label identity and connectivity using physical wire islands and hierarchical twig termination.
    Sch002,
    /// Represents the `SCH003` rule: Detects label glyphs that overlap wire interiors.
    Sch003,
    /// Represents the `SCH004` rule: Validates profile-declared physical pin connectivity and separation contracts.
    Sch004,
    /// Represents the `SCH005` rule: Checks that connected wires align with the axis of the embedded symbol pin.
    Sch005,
    /// Represents the `SCH006` rule: Enforces minimum spacing between the placed origins of group members.
    Sch006,
    /// Represents the `SCH007` rule: Checks that profile-declared members match the native `KiCad` schematic group.
    Sch007,
    /// Represents the `SCH008` rule: Checks annotation text-box geometry, required margins, overlap, and wire penetration.
    Sch008,
    /// Represents the `SCH009` rule: Checks the containment of group members and associated schematic geometry.
    Sch009,
    /// Represents the `SCH010` rule: Checks visible symbol-field bounds against nearby fields, symbols, wires, pins, labels, and annotation boxes.
    Sch010,
    /// Represents the `SCH011` rule: Requires multiline block annotations to have a visible border.
    Sch011,
    /// Represents the `SCH012` rule: Checks group content against a frame's configured inset clearance.
    Sch012,
    /// Represents the `SCH013` rule: Checks wire clearance from schematic obstacles using resolved physical geometry and effective widths.
    Sch013,
    /// Represents the `SCH014` rule: Validates profile-declared shared pin/net contracts using the exported netlist.
    Sch014,
    /// Represents the `SCH015` rule: Checks explicit no-connect markers against transformed embedded symbol pins.
    Sch015,
    /// Represents the `SCH016` rule: Checks a group's total in-frame wire length against its declared budget.
    Sch016,
    /// Represents the `SCH017` rule: Validates group membership, description references, and documentation evidence.
    Sch017,
    /// Represents the `SCH018` rule: Detects overlap between visible label glyph bounds.
    Sch018,
    /// Represents the `SCH019` rule: Detects reuse of the same label text with inconsistent label kinds.
    Sch019,
    /// Represents the `SCH020` rule: Checks profile-declared component roles, purposes, and rationale text.
    Sch020,
    /// Represents the `SCH021` rule: Checks strict component metadata and role-specific electrical connection contracts.
    Sch021,
    /// Represents the `SCH022` rule: Verifies decoupling target pins, capacitor terminals, return nets, and shared target clusters.
    Sch022,
    /// Represents the `SCH023` rule: Checks profile-declared required sheet title-block fields.
    Sch023,
    /// Represents the `SCH024` rule: Checks duplicate UUIDs within the currently loaded schematic document.
    Sch024,
    /// Represents the `PCB001` rule: Checks declared decoupling target-to-capacitor pad-center distance on the placed PCB.
    Pcb001,
    /// Represents the `SCH025` rule: Requires profile-declared test points for project-critical nets.
    Sch025,
    /// Represents the `PCB009` rule: Verifies required test-point pads exist, match their nets, and have exposed copper/mask layers.
    Pcb009,
    /// Represents the `SCH026` rule: Checks selected schematic anchors against a profile-declared project grid.
    Sch026,
    /// Represents the `SCH027` rule: Enforces profile-declared global-label allow/deny lists and required label scopes on the loaded sheet.
    Sch027,
    /// Checks UUID-linked frame title tabs, title containment, and minimum group-frame spacing.
    Sch028,
}

impl BuiltinRuleId {
    /// All built-in rules in stable identifier order.
    pub const ALL: [Self; 30] = [
        Self::Sch001,
        Self::Sch002,
        Self::Sch003,
        Self::Sch004,
        Self::Sch005,
        Self::Sch006,
        Self::Sch007,
        Self::Sch008,
        Self::Sch009,
        Self::Sch010,
        Self::Sch011,
        Self::Sch012,
        Self::Sch013,
        Self::Sch014,
        Self::Sch015,
        Self::Sch016,
        Self::Sch017,
        Self::Sch018,
        Self::Sch019,
        Self::Sch020,
        Self::Sch021,
        Self::Sch022,
        Self::Sch023,
        Self::Sch024,
        Self::Pcb001,
        Self::Sch025,
        Self::Pcb009,
        Self::Sch026,
        Self::Sch027,
        Self::Sch028,
    ];

    /// Returns the canonical external code for this built-in rule, preserving the stable `SCHnnn` identifier.
    ///
    /// # Returns
    ///
    /// The canonical string code for this rule, such as `SCH001`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sch001 => "SCH001",
            Self::Sch002 => "SCH002",
            Self::Sch003 => "SCH003",
            Self::Sch004 => "SCH004",
            Self::Sch005 => "SCH005",
            Self::Sch006 => "SCH006",
            Self::Sch007 => "SCH007",
            Self::Sch008 => "SCH008",
            Self::Sch009 => "SCH009",
            Self::Sch010 => "SCH010",
            Self::Sch011 => "SCH011",
            Self::Sch012 => "SCH012",
            Self::Sch013 => "SCH013",
            Self::Sch014 => "SCH014",
            Self::Sch015 => "SCH015",
            Self::Sch016 => "SCH016",
            Self::Sch017 => "SCH017",
            Self::Sch018 => "SCH018",
            Self::Sch019 => "SCH019",
            Self::Sch020 => "SCH020",
            Self::Sch021 => "SCH021",
            Self::Sch022 => "SCH022",
            Self::Sch023 => "SCH023",
            Self::Sch024 => "SCH024",
            Self::Pcb001 => "PCB001",
            Self::Sch025 => "SCH025",
            Self::Pcb009 => "PCB009",
            Self::Sch026 => "SCH026",
            Self::Sch027 => "SCH027",
            Self::Sch028 => "SCH028",
        }
    }

    /// Looks up an exact built-in rule code. Unknown strings are not converted into a built-in identifier.
    ///
    /// # Arguments
    ///
    /// * `value` — Exact external rule code to look up; matching is case-sensitive (for example, `SCH013`).
    ///
    /// # Returns
    ///
    /// The matching built-in rule ID, or `None` when `value` is not a known code.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }
}

impl fmt::Display for BuiltinRuleId {
    /// Writes this rule's canonical `SCHnnn` code to the supplied formatter.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Formatter receiving the stable textual representation.
    ///
    /// # Returns
    ///
    /// A formatting result indicating whether the formatter accepted the canonical rule code.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<BuiltinRuleId> for String {
    /// Converts the typed built-in rule identifier to its canonical owned string code.
    ///
    /// # Arguments
    ///
    /// * `value` — Typed built-in rule identifier whose canonical `SCHnnn` code is being converted to an owned string.
    ///
    /// # Returns
    ///
    /// An owned `String` containing the canonical `SCHnnn` code.
    fn from(value: BuiltinRuleId) -> Self {
        value.as_str().to_owned()
    }
}

impl FromStr for BuiltinRuleId {
    /// Error returned when a string does not match any built-in rule identifier.
    type Err = UnknownBuiltinRuleId;
    /// Parses an exact built-in rule code and rejects identifiers that are not present in the rule catalogue.
    ///
    /// # Arguments
    ///
    /// * `value` — Exact external rule code to parse; an unrecognized code produces `UnknownBuiltinRuleId`.
    ///
    /// # Returns
    ///
    /// The matching rule ID, or `UnknownBuiltinRuleId` when the code is unrecognized.
    ///
    /// # Errors
    ///
    /// Returns `UnknownBuiltinRuleId` when the supplied code is not a built-in rule identifier.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or_else(|| UnknownBuiltinRuleId(value.to_owned()))
    }
}

/// An unrecognized built-in rule code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownBuiltinRuleId(
    /// Unrecognized stable rule code supplied by the caller.
    pub String,
);

impl fmt::Display for UnknownBuiltinRuleId {
    /// Writes an error description containing the original unrecognized rule code to the supplied formatter.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Formatter receiving the stable textual representation.
    ///
    /// # Returns
    ///
    /// A formatting result indicating whether the formatter accepted the unknown-code description.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown built-in rule identifier {:?}", self.0)
    }
}

impl std::error::Error for UnknownBuiltinRuleId {}
#[cfg(test)]
/// Unit tests for stable built-in rule-code parsing and formatting.
mod tests {
    use std::str::FromStr;

    use super::BuiltinRuleId;
    /// Checks that every built-in rule code is unique and parses back to the same enum variant.
    #[test]
    fn every_builtin_rule_has_a_unique_round_trippable_code() {
        let codes: std::collections::BTreeSet<_> = BuiltinRuleId::ALL
            .into_iter()
            .map(BuiltinRuleId::as_str)
            .collect();
        assert_eq!(codes.len(), BuiltinRuleId::ALL.len());
        for id in BuiltinRuleId::ALL {
            assert_eq!(BuiltinRuleId::from_str(id.as_str()), Ok(id));
        }
    }
    /// Checks that unrecognized rule strings are rejected instead of being mapped to a built-in ID.
    #[test]
    fn unknown_codes_are_not_silently_treated_as_builtin_rules() {
        assert!(BuiltinRuleId::parse("SCH999").is_none());
        assert!(BuiltinRuleId::from_str("SCH999").is_err());
    }
}
