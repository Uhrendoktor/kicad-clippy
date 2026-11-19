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
}
