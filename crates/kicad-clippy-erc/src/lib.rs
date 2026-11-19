#![forbid(unsafe_code)]

//! Native `KiCad` Electrical Rules Checker adapter.
/// Native netlist export and S-expression parsing, including component, net, and pin lookup types.
mod netlist;

pub use netlist::{
    Netlist, NetlistChain, NetlistComponent, NetlistError, NetlistNet, NetlistNode, PinRef,
    export_netlist, parse_netlist_sexpr,
};

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use kicad_clippy_core::{Diagnostic, RelatedLocation, SchematicLocation, Severity};
use serde_json::Value;
use tempfile::tempdir;
use thiserror::Error;

/// Normalized result from `KiCad`'s native schematic ERC.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeErcReport {
    /// `KiCad` version reported by the CLI.
    pub kicad_version: Option<String>,
    /// Report date, when present.
    pub date: Option<String>,
    /// Coordinate unit label supplied by the report.
    pub coordinate_units: Option<String>,
    /// Checks disabled in the `KiCad` project.
    pub ignored_checks: Vec<IgnoredCheck>,
    /// Findings normalized to the shared diagnostic model.
    pub diagnostics: Vec<Diagnostic>,
}

impl NativeErcReport {
    /// Reports whether the parsed ERC report contains at least one non-excluded error diagnostic.
    ///
    /// # Returns
    ///
    /// `true` when at least one diagnostic has severity `Error` and is not excluded.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_active_error)
    }
}

/// A native ERC check intentionally disabled in the `KiCad` project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgnoredCheck {
    /// `KiCad` check key.
    pub key: String,
    /// Description supplied by `KiCad`.
    pub description: String,
}

/// Errors raised when invoking `KiCad` or decoding its ERC report.
#[derive(Debug, Error)]
pub enum ErcError {
    /// The `KiCad` executable could not be started.
    #[error("could not start KiCad CLI {program}: {source}")]
    Spawn {
        /// Executable path or command name.
        program: PathBuf,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// `KiCad` returned an unsuccessful process status.
    #[error("KiCad ERC failed with status {status}: {details}")]
    CliFailed {
        /// Exit status rendered as text.
        status: String,
        /// Combined standard output and standard error from `KiCad`.
        details: String,
    },
    /// A temporary report or input file operation failed.
    #[error("ERC report I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The JSON report could not be decoded.
    #[error("could not decode KiCad ERC JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The JSON did not match `KiCad`'s ERC report structure.
    #[error("invalid KiCad ERC report: {0}")]
    InvalidReport(String),
}

/// Runs `kicad-cli sch erc` in JSON mode, reads its report, and normalizes the sheet violations.
///
/// # Arguments
///
/// * `program` — Path or command name of the `kicad-cli` executable to run.
/// * `schematic_path` — Schematic file to pass to the KiCad command.
///
/// # Returns
///
/// The normalized ERC report, or an error when KiCad cannot run or its output cannot be read or decoded.
///
/// # Errors
///
/// Returns `Spawn` when the process cannot start, `CliFailed` when a failed invocation produces no report, `Io` for report-file errors, or `Json`/`InvalidReport` for malformed report data.
pub fn run_native_erc(
    program: impl AsRef<Path>,
    schematic_path: impl AsRef<Path>,
) -> Result<NativeErcReport, ErcError> {
    let program = program.as_ref();
    let schematic_path = schematic_path.as_ref();
    let directory = tempdir()?;
    let report_path = directory.path().join("erc-report.json");

    let output = Command::new(program)
        .arg("sch")
        .arg("erc")
        .arg("--format")
        .arg("json")
        .arg("--severity-all")
        .arg("--output")
        .arg(&report_path)
        .arg(schematic_path)
        .output()
        .map_err(|source| ErcError::Spawn {
            program: program.to_path_buf(),
            source,
        })?;

    let contents = match fs::read(&report_path) {
        Ok(contents) => contents,
        Err(source) if !output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");

            return Err(ErcError::CliFailed {
                status: output.status.to_string(),
                details: if details.is_empty() {
                    source.to_string()
                } else {
                    details
                },
            });
        }
        Err(source) => return Err(ErcError::Io(source)),
    };

    // KiCad may return a non-zero status and still write a complete ERC report.
    // In that case the report is authoritative; normalized diagnostic severity
    // determines the linter's exit code.
    parse_erc_json(&contents, schematic_path)
}

/// Decodes KiCad's per-sheet ERC JSON structure and converts its violations and ignored checks into the shared report model.
///
/// # Arguments
///
/// * `contents` — UTF-8 JSON or S-expression source text returned by KiCad.
/// * `schematic_path` — Schematic file to pass to the KiCad command.
///
/// # Returns
///
/// A report containing KiCad version/date metadata, disabled checks, and normalized diagnostics.
///
/// # Errors
///
/// Returns `Json` when the input is malformed JSON, or `InvalidReport` when the required top-level `sheets` array is absent or has the wrong type.
pub fn parse_erc_json(
    contents: &[u8],
    schematic_path: impl AsRef<Path>,
) -> Result<NativeErcReport, ErcError> {
    let root: Value = serde_json::from_slice(contents)?;
    let sheets = root
        .get("sheets")
        .and_then(Value::as_array)
        .ok_or_else(|| ErcError::InvalidReport("missing sheets array".to_owned()))?;
    let source_path = schematic_path.as_ref();

    let mut diagnostics = Vec::new();
    for sheet in sheets {
        let sheet_path = string_member(sheet, "path");
        let Some(violations) = sheet.get("violations").and_then(Value::as_array) else {
            continue;
        };

        for violation in violations {
            diagnostics.push(parse_violation(violation, sheet_path.clone(), source_path));
        }
    }

    let ignored_checks = root
        .get("ignored_checks")
        .and_then(Value::as_array)
        .map(|checks| {
            checks
                .iter()
                .map(|check| IgnoredCheck {
                    key: string_member(check, "key").unwrap_or_default(),
                    description: string_member(check, "description").unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(NativeErcReport {
        kicad_version: string_member(&root, "kicad_version"),
        date: string_member(&root, "date"),
        coordinate_units: string_member(&root, "coordinate_units"),
        ignored_checks,
        diagnostics,
    })
}
/// Converts one KiCad violation object into a normalized diagnostic, retaining its sheet path, source path, and affected-item context.
///
/// # Arguments
///
/// * `violation` — JSON object representing one native ERC violation.
/// * `sheet_path` — Optional hierarchical sheet path inherited from the containing sheet entry.
/// * `source_path` — Schematic source path attached to the normalized diagnostic.
///
/// # Returns
///
/// A diagnostic carrying the rule code, severity, source path, sheet path, and related item locations found in the violation.
fn parse_violation(
    violation: &Value,
    sheet_path: Option<String>,
    source_path: &Path,
) -> Diagnostic {
    let violation_type = string_member(violation, "type").unwrap_or_else(|| "unknown".to_owned());
    let severity_text = string_member(violation, "severity").unwrap_or_default();
    let severity = match severity_text.as_str() {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Note,
    };
    let items = violation
        .get("items")
        .and_then(Value::as_array)
        .map_or(&[][..], |values| values.as_slice());
    let related = items.iter().map(parse_related_location).collect();
    let schematic_location = items.iter().find_map(parse_schematic_location);
    let excluded = violation
        .get("excluded")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut diagnostic = Diagnostic::new(
        format!("KICAD/ERC/{violation_type}"),
        severity,
        string_member(violation, "description").unwrap_or_else(|| "KiCad ERC violation".to_owned()),
    );
    diagnostic.source = Some(source_path.to_path_buf());
    diagnostic.sheet_path = sheet_path;
    diagnostic.schematic_location = schematic_location;
    diagnostic.related = related;
    diagnostic.excluded = excluded;
    diagnostic
}
/// Extracts an affected-item description, UUID, and schematic position from one violation item.
///
/// # Arguments
///
/// * `item` — JSON object describing one affected item in an ERC violation.
///
/// # Returns
///
/// The normalized affected-item description, UUID, and schematic location; missing JSON fields remain `None` or empty.
fn parse_related_location(item: &Value) -> RelatedLocation {
    RelatedLocation {
        description: string_member(item, "description").unwrap_or_default(),
        uuid: string_member(item, "uuid"),
        location: parse_schematic_location(item),
    }
}
/// Reads the `pos` object as an `(x, y)` schematic location in millimetres.
///
/// # Arguments
///
/// * `item` — JSON object describing one affected item in an ERC violation.
///
/// # Returns
///
/// A schematic coordinate when both numeric `x` and `y` members are present; otherwise `None`.
fn parse_schematic_location(item: &Value) -> Option<SchematicLocation> {
    let position = item.get("pos")?;
    Some(SchematicLocation {
        x_mm: position.get("x")?.as_f64()?,
        y_mm: position.get("y")?.as_f64()?,
    })
}
/// Returns a JSON object member only when it contains a string value.
///
/// # Arguments
///
/// * `value` — JSON object or value containing the field to inspect.
/// * `key` — Name of the JSON member to read.
///
/// # Returns
///
/// The owned string value when the member exists and is a JSON string; otherwise `None`.
fn string_member(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(ToOwned::to_owned)
}
#[cfg(test)]
/// Unit tests for native ERC report parsing and failure handling.
mod tests {
    use std::{error::Error, path::Path};

    use kicad_clippy_core::Severity;

    use super::parse_erc_json;
    /// Representative native ERC JSON report with a nested per-sheet violation.
    const ERC_REPORT: &[u8] = br#"
    {
      "$schema": "https://schemas.kicad.org/erc.v1.json",
      "coordinate_units": "mm",
      "kicad_version": "10.0.0",
      "date": "2026-01-01T12:00:00+0100",
      "sheets": [
        {
          "path": "/",
          "uuid_path": "/root",
          "violations": [
            {
              "type": "pin_not_connected",
              "description": "Pin not connected",
              "severity": "error",
              "excluded": false,
              "items": [
                {
                  "description": "Symbol U1 Pin 5",
                  "uuid": "item-uuid",
                  "pos": {"x": 10.5, "y": 20.25}
                }
              ]
            }
          ]
        }
      ],
      "ignored_checks": [{"key": "pin_to_pin", "description": "Pin-to-pin conflict"}]
    }
    "#;
    /// Checks that nested per-sheet ERC violations are parsed and their item locations are retained as related diagnostics.
    ///
    /// # Returns
    ///
    /// `Ok(())` when nested sheet violations are normalized and their item locations are retained as related diagnostic context.
    ///
    /// # Errors
    ///
    /// Propagates `Json` for malformed report bytes or `InvalidReport` when the required sheet/violation structure is absent or malformed.
    #[test]
    fn reads_sheet_nested_violations_and_preserves_item_context() -> Result<(), Box<dyn Error>> {
        let report = parse_erc_json(ERC_REPORT, Path::new("fixture.kicad_sch"))?;

        assert_eq!(report.kicad_version.as_deref(), Some("10.0.0"));
        assert_eq!(report.diagnostics.len(), 1);
        let diagnostic = &report.diagnostics[0];
        assert_eq!(diagnostic.code, "KICAD/ERC/pin_not_connected");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.sheet_path.as_deref(), Some("/"));
        assert_eq!(
            diagnostic
                .schematic_location
                .map(|point| (point.x_mm, point.y_mm)),
            Some((10.5, 20.25))
        );
        assert_eq!(diagnostic.related[0].uuid.as_deref(), Some("item-uuid"));
        assert_eq!(report.ignored_checks.len(), 1);
        assert!(report.has_errors());
        Ok(())
    }
    /// Checks that an unrelated top-level DRC violations array is rejected when the required ERC `sheets` array is absent.
    #[test]
    fn rejects_a_drc_shaped_top_level_violations_array() {
        let drc_shaped = br#"{"violations":[{"severity":"error"}]}"#;

        assert!(parse_erc_json(drc_shaped, "fixture.kicad_sch").is_err());
    }
    /// Checks that excluded native ERC errors are preserved in the report but are not counted as active errors.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the excluded violation remains in the report but `has_errors()` returns `false`.
    ///
    /// # Errors
    ///
    /// Propagates `Json` for malformed report bytes or `InvalidReport` when the required sheet/violation structure is absent or malformed.
    #[test]
    fn excluded_errors_are_retained_but_do_not_fail_the_report() -> Result<(), Box<dyn Error>> {
        let contents = br#"{
          "sheets": [{
            "path": "/",
            "violations": [{
              "type": "example",
              "description": "Excluded check",
              "severity": "error",
              "excluded": true,
              "items": []
            }]
          }]
        }"#;
        let report = parse_erc_json(contents, "fixture.kicad_sch")?;

        assert_eq!(report.diagnostics.len(), 1);
        assert!(!report.has_errors());
        assert!(report.diagnostics[0].excluded);
        Ok(())
    }
}
