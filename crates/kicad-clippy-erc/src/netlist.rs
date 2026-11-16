use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use kiutils_sexpr::{Atom, Node, ParseError, parse_one};
use tempfile::tempdir;
use thiserror::Error;

/// A reference/pin pair in a flattened KiCad netlist.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PinRef {
    /// Component reference.
    pub reference: String,
    /// Pin number as written by KiCad.
    pub pin: String,
}

/// A component exported by KiCad.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistComponent {
    /// Reference designator.
    pub reference: String,
    /// Resolved component value.
    pub value: Option<String>,
    /// Library identifier, when present.
    pub lib_id: Option<String>,
}

/// One pin attached to a net.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistNode {
    /// Component reference.
    pub reference: String,
    /// Pin number.
    pub pin: String,
}

/// One logical net and its attached component pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistNet {
    /// KiCad net name.
    pub name: String,
    /// Pins assigned to this net.
    pub nodes: Vec<NetlistNode>,
}

/// Flattened netlist exported by KiCad.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Netlist {
    /// Components keyed by reference designator.
    pub components: BTreeMap<String, NetlistComponent>,
    /// Nets keyed by KiCad net name.
    pub nets: BTreeMap<String, NetlistNet>,
    /// Reverse lookup from component pin to net name.
    pub pin_nets: BTreeMap<PinRef, String>,
}

impl Netlist {
    /// Returns the net name connected to a component pin.
    #[must_use]
    pub fn net_for_pin(&self, reference: &str, pin: &str) -> Option<&str> {
        self.pin_nets
            .get(&PinRef {
                reference: reference.to_owned(),
                pin: pin.to_owned(),
            })
            .map(String::as_str)
    }

    /// Returns the pins attached to a named net.
    #[must_use]
    pub fn pins_on_net(&self, name: &str) -> Option<&[NetlistNode]> {
        self.nets.get(name).map(|net| net.nodes.as_slice())
    }
}

/// Errors raised while exporting or parsing a KiCad netlist.
#[derive(Debug, Error)]
pub enum NetlistError {
    /// The KiCad executable could not be started.
    #[error("could not start KiCad CLI {program}: {source}")]
    Spawn {
        /// Executable path or command name.
        program: PathBuf,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// KiCad returned a failure without a usable netlist.
    #[error("KiCad netlist export failed with status {status}: {details}")]
    CliFailed {
        /// Exit status rendered as text.
        status: String,
        /// Combined process output.
        details: String,
    },
    /// Netlist file I/O failed.
    #[error("netlist I/O failed: {0}")]
    Io(#[from] io::Error),
    /// S-expression syntax is invalid.
    #[error("invalid KiCad netlist S-expression: {0}")]
    Syntax(#[from] ParseError),
    /// The root expression is not an export document.
    #[error("expected an export netlist root, found {0:?}")]
    WrongRoot(String),
    /// A required component or net field is missing.
    #[error("netlist is missing required field {field} in {context}")]
    MissingField {
        /// Missing field name.
        field: String,
        /// S-expression containing the missing field.
        context: String,
    },
    /// A component pin was assigned to more than one net.
    #[error("pin {reference}.{pin} appears on multiple nets")]
    DuplicatePin {
        /// Component reference.
        reference: String,
        /// Pin number.
        pin: String,
    },
}

/// Exports a flattened KiCad netlist using the CLI native S-expression format.
///
/// # Errors
///
/// Returns NetlistError when KiCad cannot be started, the command fails,
/// or the generated netlist cannot be read or parsed.
pub fn export_netlist(
    program: impl AsRef<Path>,
    schematic_path: impl AsRef<Path>,
) -> Result<Netlist, NetlistError> {
    let program = program.as_ref();
    let schematic_path = schematic_path.as_ref();
    let directory = tempdir()?;
    let netlist_path = directory.path().join("netlist.net");
    let output = Command::new(program)
        .arg("sch")
        .arg("export")
        .arg("netlist")
        .arg("--format")
        .arg("kicadsexpr")
        .arg("--output")
        .arg(&netlist_path)
        .arg(schematic_path)
        .output()
        .map_err(|source| NetlistError::Spawn {
            program: program.to_path_buf(),
            source,
        })?;

    let contents = match fs::read_to_string(&netlist_path) {
        Ok(contents) => contents,
        Err(source) if !output.status.success() => {
            return Err(NetlistError::CliFailed {
                status: output.status.to_string(),
                details: process_details(&output.stdout, &output.stderr, &source.to_string()),
            });
        }
        Err(source) => return Err(NetlistError::Io(source)),
    };
    if !output.status.success() {
        return Err(NetlistError::CliFailed {
            status: output.status.to_string(),
            details: process_details(&output.stdout, &output.stderr, ""),
        });
    }
    parse_netlist_sexpr(&contents)
}

/// Parses KiCad flattened netlist S-expression data.
///
/// # Errors
///
/// Returns NetlistError for malformed S-expressions, wrong roots,
/// missing required fields, or duplicate component-pin assignments.
pub fn parse_netlist_sexpr(contents: &str) -> Result<Netlist, NetlistError> {
    let document = parse_one(contents)?;
    let root = document
        .nodes
        .first()
        .ok_or_else(|| NetlistError::WrongRoot(String::new()))?;
    let root_name = head(root).unwrap_or_default();
    if root_name != "export" {
        return Err(NetlistError::WrongRoot(root_name.to_owned()));
    }

    let mut netlist = Netlist::default();
    if let Some(components) = child_list(root, "components") {
        for component in children(components, "comp") {
            let reference = child_value(component, "ref", contents)
                .ok_or_else(|| missing_field("ref", "component"))?;
            let value = child_value(component, "value", contents);
            let lib_id = child_list(component, "libsource").and_then(|libsource| {
                let library = child_value(libsource, "lib", contents)?;
                let part = child_value(libsource, "part", contents)?;
                Some(format!("{library}:{part}"))
            });
            let record = NetlistComponent {
                reference: reference.clone(),
                value,
                lib_id,
            };
            netlist.components.insert(reference, record);
        }
    }

    if let Some(nets) = child_list(root, "nets") {
        for net in children(nets, "net") {
            let name = child_value(net, "name", contents)
                .ok_or_else(|| missing_field("name", "net"))?;
            let mut nodes = Vec::new();
            for node in children(net, "node") {
                let reference = child_value(node, "ref", contents)
                    .ok_or_else(|| missing_field("ref", "net node"))?;
                let pin = child_value(node, "pin", contents)
                    .ok_or_else(|| missing_field("pin", "net node"))?;
                let pin_ref = PinRef {
                    reference: reference.clone(),
                    pin: pin.clone(),
                };
                if netlist.pin_nets.insert(pin_ref, name.clone()).is_some() {
                    return Err(NetlistError::DuplicatePin { reference, pin });
                }
                nodes.push(NetlistNode { reference, pin });
            }
            netlist.nets.insert(name.clone(), NetlistNet { name, nodes });
        }
    }
    Ok(netlist)
}

fn process_details(stdout: &[u8], stderr: &[u8], fallback: &str) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    [stdout.trim(), stderr.trim(), fallback]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn missing_field(field: &str, context: &str) -> NetlistError {
    NetlistError::MissingField {
        field: field.to_owned(),
        context: context.to_owned(),
    }
}

fn list_items(node: &Node) -> Option<&[Node]> {
    if let Node::List { items, .. } = node {
        Some(items)
    } else {
        None
    }
}

fn head(node: &Node) -> Option<&str> {
    let Node::List { items, .. } = node else {
        return None;
    };
    let Node::Atom {
        atom: Atom::Symbol(value),
        ..
    } = items.first()?
    else {
        return None;
    };
    Some(value)
}

fn children<'a>(node: &'a Node, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
    list_items(node)
        .into_iter()
        .flatten()
        .filter(move |child| head(child) == Some(name))
}

fn child_list<'a>(node: &'a Node, name: &'a str) -> Option<&'a Node> {
    children(node, name).next()
}

fn child_value(node: &Node, name: &str, source: &str) -> Option<String> {
    let child = child_list(node, name)?;
    let items = list_items(child)?;
    atom_value(items.get(1)?, source)
}

fn atom_value(node: &Node, source: &str) -> Option<String> {
    let Node::Atom { span, .. } = node else {
        return None;
    };
    source
        .get(span.start..span.end)
        .map(|value| value.trim_matches('"').to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::parse_netlist_sexpr;

    const NETLIST: &str = r#"(export
      (components
        (comp (ref "C1") (value "100n") (libsource (lib "Device") (part "C")))
        (comp (ref "U1") (value "Controller") (libsource (lib "MyLib") (part "U")))
      )
      (nets
        (net (code "1") (name "VREG_1V2")
          (node (ref "U1") (pin "21"))
          (node (ref "C1") (pin "1"))
        )
        (net (code "2") (name "GND")
          (node (ref "C1") (pin "2"))
        )
      )
    )"#;

    #[test]
    fn maps_component_pins_to_exported_net_names() {
        let netlist = parse_netlist_sexpr(NETLIST).expect("valid fixture");
        assert_eq!(netlist.net_for_pin("U1", "21"), Some("VREG_1V2"));
        assert_eq!(netlist.net_for_pin("C1", "1"), Some("VREG_1V2"));
        assert_eq!(netlist.net_for_pin("C1", "2"), Some("GND"));
        assert_eq!(netlist.components["C1"].value.as_deref(), Some("100n"));
        assert_eq!(netlist.components["U1"].lib_id.as_deref(), Some("MyLib:U"));
    }

    #[test]
    fn rejects_a_pin_assigned_to_two_nets() {
        let duplicate = r#"(export (nets
          (net (code "1") (name "A") (node (ref "R1") (pin "1")))
          (net (code "2") (name "B") (node (ref "R1") (pin "1")))
        ))"#;
        assert!(parse_netlist_sexpr(duplicate).is_err());
    }
}
