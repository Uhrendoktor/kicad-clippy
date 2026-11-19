use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use kiutils_sexpr::{Atom, Node, ParseError, parse_one};
use tempfile::tempdir;
use thiserror::Error;

/// A reference/pin pair in a flattened `KiCad` netlist.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PinRef {
    /// Component reference.
    pub reference: String,
    /// Pin number as written by `KiCad`.
    pub pin: String,
}

/// A component exported by `KiCad`.
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
    /// `KiCad` net name.
    pub name: String,
    /// Resolved effective netclass name exported by `KiCad`, when present.
    pub class_name: Option<String>,
    /// Pins assigned to this net.
    pub nodes: Vec<NetlistNode>,
}

/// A committed net chain exported by `KiCad`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistChain {
    /// Stable chain name.
    pub name: String,
    /// Netclass directly assigned to the chain, when present.
    pub netclass: Option<String>,
    /// Resolved member-net names.
    pub members: BTreeSet<String>,
}

/// Flattened netlist exported by `KiCad`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Netlist {
    /// Components keyed by reference designator.
    pub components: BTreeMap<String, NetlistComponent>,
    /// Nets keyed by `KiCad` net name.
    pub nets: BTreeMap<String, NetlistNet>,
    /// Reverse lookup from component pin to net name.
    pub pin_nets: BTreeMap<PinRef, String>,
    /// Committed net chains and their resolved member nets.
    pub chains: Vec<NetlistChain>,
}

impl Netlist {
    /// Looks up the logical net connected to the specified component pin.
    ///
    /// # Arguments
    ///
    /// * `reference` — Component reference designator from the flattened netlist.
    /// * `pin` — Pin number exactly as exported by `KiCad`.
    ///
    /// # Returns
    ///
    /// The associated net name, or `None` when `KiCad` did not assign the pin to an exported net.
    #[must_use]
    pub fn net_for_pin(&self, reference: &str, pin: &str) -> Option<&str> {
        self.pin_nets
            .get(&PinRef {
                reference: reference.to_owned(),
                pin: pin.to_owned(),
            })
            .map(String::as_str)
    }

    /// Returns the component pins attached to the named logical net.
    ///
    /// # Arguments
    ///
    /// * `name` — Logical net name or S-expression child name being looked up.
    ///
    /// # Returns
    ///
    /// The net's pin slice, or `None` when the net name is not present.
    #[must_use]
    pub fn pins_on_net(&self, name: &str) -> Option<&[NetlistNode]> {
        self.nets.get(name).map(|net| net.nodes.as_slice())
    }
}

/// Errors raised while exporting or parsing a `KiCad` netlist.
#[derive(Debug, Error)]
pub enum NetlistError {
    /// The `KiCad` executable could not be started.
    #[error("could not start KiCad CLI {program}: {source}")]
    Spawn {
        /// Executable path or command name.
        program: PathBuf,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// `KiCad` returned a failure without a usable netlist.
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

/// Invokes `KiCad`'s native netlist exporter and parses the resulting S-expression document.
///
/// # Arguments
///
/// * `program` — Path or command name of the `kicad-cli` executable to run.
/// * `schematic_path` — Schematic file to pass to the `KiCad` command.
///
/// # Returns
///
/// A parsed netlist, or an error describing export, file access, S-expression parsing, or inconsistent pin assignments.
///
/// # Errors
///
/// Returns `Spawn` or `CliFailed` for `KiCad` process failures, `Io` for netlist-file errors, and parsing errors when exported S-expression data is malformed or pin assignments are inconsistent.
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

/// Parses a `KiCad` `export` S-expression into component, net, chain, and reverse pin-to-net indexes.
///
/// # Arguments
///
/// * `contents` — UTF-8 `export` S-expression text produced by `KiCad`'s native netlist exporter.
///
/// # Returns
///
/// The flattened component/net model and indexes, or a structured error for malformed or inconsistent netlist data.
///
/// # Errors
///
/// Returns `Syntax` for malformed S-expressions, `WrongRoot` for a non-`export` root, `MissingField` for absent required component/net data, or `DuplicatePin` when one pin is assigned to multiple nets.
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
            let name =
                child_value(net, "name", contents).ok_or_else(|| missing_field("name", "net"))?;
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
            let class_name =
                child_value(net, "class", contents).filter(|class_name| !class_name.is_empty());
            netlist.nets.insert(
                name.clone(),
                NetlistNet {
                    name,
                    class_name,
                    nodes,
                },
            );
        }
    }

    if let Some(chains) = child_list(root, "net_chains") {
        for chain in children(chains, "net_chain") {
            let Some(name) = child_value(chain, "name", contents) else {
                continue;
            };
            let netclass =
                child_value(chain, "net_class", contents).filter(|netclass| !netclass.is_empty());
            let members = child_list(chain, "members").map_or_else(BTreeSet::new, |member_list| {
                children(member_list, "member")
                    .filter_map(|member| child_value(member, "net", contents))
                    .collect()
            });
            netlist.chains.push(NetlistChain {
                name,
                netclass,
                members,
            });
        }
    }
    Ok(netlist)
}
/// Combines non-empty process output streams with a fallback error description for a failed CLI operation.
///
/// # Arguments
///
/// * `stdout` — Standard output bytes captured from the failed process.
/// * `stderr` — Standard error bytes captured from the failed process.
/// * `fallback` — Additional error text to include when neither process stream explains the failure.
///
/// # Returns
///
/// A newline-separated diagnostic string containing the available process output and fallback context.
fn process_details(stdout: &[u8], stderr: &[u8], fallback: &str) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    [stdout.trim(), stderr.trim(), fallback]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
/// Constructs a structured parse error naming the required field and the S-expression context where it was absent.
///
/// # Arguments
///
/// * `field` — Required field name used in the parse error.
/// * `context` — S-expression object description in which the required field was missing.
///
/// # Returns
///
/// A `NetlistError::MissingField` describing the missing member and its containing object.
fn missing_field(field: &str, context: &str) -> NetlistError {
    NetlistError::MissingField {
        field: field.to_owned(),
        context: context.to_owned(),
    }
}
/// Returns the children of an S-expression list node.
///
/// # Arguments
///
/// * `node` — S-expression node whose children or head symbol are being inspected.
///
/// # Returns
///
/// A borrowed slice of list children, or `None` when the node is not a list.
fn list_items(node: &Node) -> Option<&[Node]> {
    if let Node::List { items, .. } = node {
        Some(items)
    } else {
        None
    }
}
/// Returns the symbol at the head of an S-expression list.
///
/// # Arguments
///
/// * `node` — S-expression node whose children or head symbol are being inspected.
///
/// # Returns
///
/// The leading symbol of a list node, or `None` for atoms and malformed lists.
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
/// Iterates over direct child lists whose head matches the requested name.
///
/// # Arguments
///
/// * `node` — S-expression node whose children or head symbol are being inspected.
/// * `name` — Logical net name or S-expression child name being looked up.
///
/// # Returns
///
/// An iterator over matching direct child-list nodes.
fn children<'a>(node: &'a Node, name: &str) -> impl Iterator<Item = &'a Node> + 'a {
    let name = name.to_owned();
    list_items(node)
        .into_iter()
        .flatten()
        .filter(move |child| head(child) == Some(name.as_str()))
}
/// Returns the first direct child list with the requested head symbol.
///
/// # Arguments
///
/// * `node` — S-expression node whose children or head symbol are being inspected.
/// * `name` — Logical net name or S-expression child name being looked up.
///
/// # Returns
///
/// The first matching child-list node, or `None` when no such child exists.
fn child_list<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    children(node, name).next()
}
/// Reads and decodes the first atom value in a named child list using the original source text.
///
/// # Arguments
///
/// * `node` — S-expression node whose children or head symbol are being inspected.
/// * `name` — Logical net name or S-expression child name being looked up.
/// * `source` — Original netlist source text used to decode the raw atom span.
///
/// # Returns
///
/// The decoded child string, or `None` when the child or its atom value is absent.
fn child_value(node: &Node, name: &str, source: &str) -> Option<String> {
    let child = child_list(node, name)?;
    let items = list_items(child)?;
    atom_value(items.get(1)?, source)
}
/// Decodes the source text covered by an atom's span, preserving the original token's escaping rules.
///
/// # Arguments
///
/// * `node` — S-expression node whose children or head symbol are being inspected.
/// * `source` — Original netlist source text used to decode the raw atom span.
///
/// # Returns
///
/// The decoded string represented by the atom, or `None` when the node is not a quoted/symbol atom with a valid source span.
fn atom_value(node: &Node, source: &str) -> Option<String> {
    let Node::Atom { span, .. } = node else {
        return None;
    };
    source
        .get(span.start..span.end)
        .map(|value| value.trim_matches('"').to_owned())
}
#[cfg(test)]
/// Unit tests for native netlist export parsing and pin-to-net indexing.
mod tests {
    #![allow(clippy::expect_used)]
    use super::parse_netlist_sexpr;
    use std::collections::BTreeSet;
    /// Minimal exported netlist fixture used to test component, net, and pin relationships.
    const NETLIST: &str = r#"(export
      (components
        (comp (ref "C1") (value "100n") (libsource (lib "Device") (part "C")))
        (comp (ref "U1") (value "Controller") (libsource (lib "MyLib") (part "U")))
      )
      (nets
        (net (code "1") (name "VREG_1V2") (class "HighSpeed")
          (node (ref "U1") (pin "21"))
          (node (ref "C1") (pin "1"))
        )
        (net (code "2") (name "GND")
          (node (ref "C1") (pin "2"))
        )
      )
      (net_chains
        (net_chain (name "Core") (net_class "HighSpeed")
          (members (member (net "VREG_1V2")) (member (net "GND")))
        )
      )
    )"#;
    /// Checks that each component reference/pin pair is indexed to the logical net exported by `KiCad`.
    #[test]
    fn maps_component_pins_to_exported_net_names() {
        let netlist = parse_netlist_sexpr(NETLIST).expect("valid fixture");
        assert_eq!(netlist.net_for_pin("U1", "21"), Some("VREG_1V2"));
        assert_eq!(netlist.net_for_pin("C1", "1"), Some("VREG_1V2"));
        assert_eq!(netlist.net_for_pin("C1", "2"), Some("GND"));
        assert_eq!(
            netlist.nets["VREG_1V2"].class_name.as_deref(),
            Some("HighSpeed")
        );
        assert_eq!(netlist.chains[0].name, "Core");
        assert_eq!(netlist.chains[0].netclass.as_deref(), Some("HighSpeed"));
        assert_eq!(
            netlist.chains[0].members,
            BTreeSet::from(["VREG_1V2".to_owned(), "GND".to_owned()])
        );
        assert_eq!(netlist.components["C1"].value.as_deref(), Some("100n"));
        assert_eq!(netlist.components["U1"].lib_id.as_deref(), Some("MyLib:U"));
    }
    /// Checks that a component pin appearing on multiple exported nets is rejected as an inconsistent netlist.
    #[test]
    fn rejects_a_pin_assigned_to_two_nets() {
        let duplicate = r#"(export (nets
          (net (code "1") (name "A") (node (ref "R1") (pin "1")))
          (net (code "2") (name "B") (node (ref "R1") (pin "1")))
        ))"#;
        assert!(parse_netlist_sexpr(duplicate).is_err());
    }
}
