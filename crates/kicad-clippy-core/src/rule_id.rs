use std::{fmt, str::FromStr};

/// Stable identifier for a built-in schematic rule.
///
/// Built-in identifiers are represented as an enum in Rust code so call sites
/// do not need to repeat string literals. Their string representation remains
/// the public, serialized diagnostic code (for example, `SCH001`).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BuiltinRuleId {
    /// Represents the `Sch001` case.
    Sch001,
    /// Represents the `Sch002` case.
    Sch002,
    /// Represents the `Sch003` case.
    Sch003,
    /// Represents the `Sch004` case.
    Sch004,
    /// Represents the `Sch005` case.
    Sch005,
    /// Represents the `Sch006` case.
    Sch006,
    /// Represents the `Sch007` case.
    Sch007,
    /// Represents the `Sch008` case.
    Sch008,
    /// Represents the `Sch009` case.
    Sch009,
    /// Represents the `Sch010` case.
    Sch010,
    /// Represents the `Sch011` case.
    Sch011,
    /// Represents the `Sch012` case.
    Sch012,
    /// Represents the `Sch013` case.
    Sch013,
    /// Represents the `Sch014` case.
    Sch014,
    /// Represents the `Sch015` case.
    Sch015,
    /// Represents the `Sch016` case.
    Sch016,
    /// Represents the `Sch017` case.
    Sch017,
    /// Represents the `Sch018` case.
    Sch018,
    /// Represents the `Sch019` case.
    Sch019,
    /// Represents the `Sch020` case.
    Sch020,
    /// Represents the `Sch021` case.
    Sch021,
    /// Represents the `Sch022` case.
    Sch022,
}

impl BuiltinRuleId {
    /// All built-in rules in stable identifier order.
    pub const ALL: [Self; 22] = [
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
    ];

    /// Returns the stable external code for this rule.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
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
        }
    }

    /// Parses a stable built-in rule code without accepting unknown identifiers.
    /// # Arguments
    ///
    /// * `value` — Input `value` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }
}

impl fmt::Display for BuiltinRuleId {
    /// Processes fmt using the supplied values and schematic data.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Input `formatter` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<BuiltinRuleId> for String {
    /// Processes from using the supplied values and schematic data.
    ///
    /// # Arguments
    ///
    /// * `value` — Input `value` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    fn from(value: BuiltinRuleId) -> Self {
        value.as_str().to_owned()
    }
}

impl FromStr for BuiltinRuleId {
    /// Defines the err type used by this crate.
    type Err = UnknownBuiltinRuleId;
    /// Processes from str using the supplied values and schematic data.
    ///
    /// # Arguments
    ///
    /// * `value` — Input `value` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the operation cannot complete with the supplied input.
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
    /// Processes fmt using the supplied values and schematic data.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Input `formatter` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown built-in rule identifier {:?}", self.0)
    }
}

impl std::error::Error for UnknownBuiltinRuleId {}
/// Contains the tests implementation and supporting types.

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::BuiltinRuleId;
    /// Processes every builtin rule has a unique round trippable code using the supplied values and schematic data.
    ///

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
    /// Processes unknown codes are not silently treated as builtin rules using the supplied values and schematic data.
    ///

    #[test]
    fn unknown_codes_are_not_silently_treated_as_builtin_rules() {
        assert!(BuiltinRuleId::parse("SCH999").is_none());
        assert!(BuiltinRuleId::from_str("SCH999").is_err());
    }
}
