use std::collections::{BTreeMap, BTreeSet};

use kicad_clippy_core::{
    Diagnostic, RuleDescriptor, RuleLevel, RuleRegistry, Severity,
    SchematicLocation,
};
use kicad_clippy_parser::SchematicDocument;
use kiutils_sexpr::Node;

use crate::{
    AdjacencyContract, CommonNetContract, ComponentKind, ComponentMetadata, ComponentRole,
    DesignGroup, DesignProfile, PinPair, RuleConfig,
    RuleConfigError,
    model::{
        Point, Rect, SchematicModel, Segment, TOL, atom_at, direct_nodes,
        line_hits_rect, point_on_segment, primitive_intersects_segment, span,
    },
};

const RULES: &[(&str, &str, &str, &str, &str, RuleLevel)] = &[
    ("SCH001", "wire-geometry", "geometry", "Require orthogonal, non-overlapping wire segments and explicit junctions.", "Wire geometry is the visible record of connectivity. Diagonal routes, crossings and ambiguous branches make both electrical intent and review harder to trust.", RuleLevel::Deny),
    ("SCH002", "label-topology", "connectivity", "Require local labels to connect distinct wire islands and hierarchical labels to terminate a wire twig.", "A local label is a reusable net connection, not decoration on a continuous wire. Hierarchical labels should sit at clear child-sheet interface endpoints.", RuleLevel::Deny),
    ("SCH003", "label-wire-clearance", "layout", "Prevent wire segments from passing through visible label glyphs.", "A wire through label text makes the net name difficult to read and can obscure the actual connection anchor.", RuleLevel::Deny),
    ("SCH004", "physical-net-connectivity", "profile", "Enforce explicitly declared direct pin-to-pin wire paths.", "Certain relationships, such as regulator bypasses or a MOSFET common-source node, are easier to review as short physical wires. A same-name label must not silently substitute for a required direct connection.", RuleLevel::Deny),
    ("SCH005", "pin-orientation", "profile", "Enforce declared ground-pin and ground-port orientation.", "Consistent power-symbol orientation improves scanability and helps communicate the direction of a return connection.", RuleLevel::Deny),
    ("SCH006", "component-spacing", "profile", "Enforce declared component alignment, adjacency and property offsets.", "Related passives should share clear axes and compact spacing; references and values should stay close to the components they describe.", RuleLevel::Warn),
    ("SCH007", "group-metadata", "profile", "Require every declared functional group to have a title, description and frame.", "Functional grouping should be explicit and machine-checkable, rather than inferred from floating captions or an oversized page enclosure.", RuleLevel::Deny),
    ("SCH008", "annotation-layout", "annotation", "Require measured native text boxes with consistent padding and no overlaps.", "Text should fit its box, retain a readable margin and avoid wires or other annotations. Free-standing text is disallowed by the style contract.", RuleLevel::Deny),
    ("SCH009", "group-containment", "profile", "Keep declared group members, pins, labels and notes within their owning functional frame.", "A frame should describe a real functional module. Wires, labels and fields must not cross its border or drift into a neighbouring group.", RuleLevel::Deny),
    ("SCH010", "symbol-field-clearance", "geometry", "Keep reference and value fields clear of symbol geometry, pins, wires and other text.", "Fields that float far from symbols or overlap bodies, pins, labels or wires weaken readability and can hide electrical relationships.", RuleLevel::Deny),
    ("SCH011", "block-annotation-structure", "annotation", "Require multiline notes and declared group descriptions to have visible borders.", "Multiline explanatory notes need a visible boundary. Group notes should sit inside their functional region rather than appear as unrelated floating text.", RuleLevel::Deny),
    ("SCH012", "frame-content-clearance", "profile", "Keep visible text and description panels inset from functional-frame borders.", "A consistent interior margin keeps frame tabs and borders from competing with the text they are intended to organize.", RuleLevel::Deny),
    ("SCH013", "wire-obstacle-clearance", "geometry", "Prevent wires from crossing symbol outlines, intermediate pins, fields or annotation boxes.", "A wire may terminate at its intended pin, but must not pass through the body or another pin of a symbol. Obstacle checks use embedded symbol primitives rather than only enclosing rectangles.", RuleLevel::Deny),
    ("SCH014", "shared-pin-net-contracts", "profile", "Check configured common-net membership, physical connection and label count.", "A declared shared net should agree with both pin-level connectivity and the expected logical label, with no accidental extra label on the same physical island.", RuleLevel::Deny),
    ("SCH015", "required-no-connects", "profile", "Require explicit no-connect markers on declared intentionally unused pins.", "An intentionally unused pin should be visibly marked rather than left ambiguous. Embedded library pins already marked unconnected must not be mistaken for placed X markers.", RuleLevel::Deny),
    ("SCH016", "wire-compactness", "profile", "Score internal wire length against a per-group routing budget.", "A length budget is a review signal for unnecessary detours, not a routing optimizer. It should prompt inspection rather than blind coordinate changes.", RuleLevel::Warn),
    ("SCH017", "circuit-documentation", "documentation", "Require functional groups to identify their purpose and source evidence.", "A reviewer should be able to understand why a circuit block exists and trace datasheet-derived choices to a source, page and figure or a reviewed image.", RuleLevel::Deny),
    ("SCH018", "label-text-collision", "layout", "Prevent visible labels from overlapping labels, fields and annotation boxes.", "Label glyph overlap can make a valid netlist visually ambiguous. Check the rendered text bounds, not only anchor points.", RuleLevel::Deny),
    ("SCH019", "label-kind-consistency", "connectivity", "Require a given net name to use one label kind consistently.", "Mixing local, hierarchical and global forms of the same net name creates hidden scope and makes block boundaries difficult to reason about.", RuleLevel::Deny),
    ("SCH020", "component-purpose-documentation", "documentation", "Require documented components to state both their electrical function and why they are present.", "Naming a component is not enough. Important resistors and support parts should explain their function and their engineering rationale in the associated group note.", RuleLevel::Deny),
    ("SCH021", "symbol-grid-alignment", "layout", "Warn when placed symbol origins do not align to the design profile grid.", "Grid-aligned component rows and shared rails create visual consistency. This check is separate from KiCad's native connection-grid ERC for pins and wire endpoints.", RuleLevel::Warn),
    ("SCH022", "unique-object-uuids", "integrity", "Require schematic object UUIDs to be unique within the document.", "KiCad uses UUIDs to identify schematic objects and hierarchy instances. Duplicate identifiers can make selection, references and synchronization ambiguous.", RuleLevel::Deny),
    ("SCH023", "title-block-completeness", "documentation", "Require a non-empty schematic title and any configured title-block fields.", "The native title block is the formal document identity. It should carry the drawing title and the project-specific revision/date/company data selected by the profile.", RuleLevel::Warn),
    ("SCH024", "component-intent-metadata", "metadata", "Require typed metadata for selected component types and validate declared role/function/rationale.", "Typed design intent connects a component to the circuit role it serves. Strict projects can require capacitors to state whether they are bulk, decoupling, filtering, coupling or another role, with an explicit target pin for decouplers.", RuleLevel::Warn),
    ("SCH025", "decoupling-network-contracts", "electrical-intent", "Validate decoupling-capacitor target pins, proximity, physical connection and shared return networks.", "A capacitor tagged as a decoupler must actually connect to the declared IC pin. Decouplers serving the same pin should form a compact group and share the intended return network; KiCad ERC alone cannot infer this design intent.", RuleLevel::Deny),
];

/// Returns the built-in rule catalogue in stable identifier order.
///
/// # Errors
///
/// Returns an error only if the statically declared catalogue contains duplicate identifiers.
pub fn available_rules() -> Result<RuleRegistry, RuleConfigError> {
    let mut registry = RuleRegistry::new();
    for (id, name, category, summary, explanation, level) in RULES {
        registry.register(RuleDescriptor::new(
            *id, *name, *category, *summary, *explanation, *level,
        ))?;
    }
    Ok(registry)
}

/// Runs generic rules and, when configured, the profile-specific contracts.
///
/// # Errors
///
/// Returns an error when the configuration schema or preset is unsupported.
pub fn run_rules(
    document: &SchematicDocument,
    config: &RuleConfig,
) -> Result<Vec<Diagnostic>, RuleConfigError> {
    config.validate()?;
    let model = SchematicModel::new(document);
    let mut sink = RuleSink {
        document,
        model: &model,
        config,
        diagnostics: Vec::new(),
    };

    sink.run_generic_rules();
    if config.preset != "generic" {
        if let Some(profile) = &config.profile {
            sink.run_profile_rules(profile);
        }
    }
    sink.rule_symbol_field_clearance(config.profile.as_ref());
    sink.rule_multiline_annotations(config.profile.as_ref());
    sink.rule_symbol_grid(config.profile.as_ref().and_then(|profile| profile.grid_mm));
    sink.rule_title_block(config.profile.as_ref());
    if config.preset != "generic" {
        sink.rule_component_metadata(config.profile.as_ref());
        sink.rule_decoupling_networks(config.profile.as_ref());
    }
    Ok(sink.diagnostics)
}

struct RuleSink<'a, 'm> {
    document: &'a SchematicDocument,
    model: &'m SchematicModel<'a>,
    config: &'a RuleConfig,
    diagnostics: Vec<Diagnostic>,
}

impl RuleSink<'_, '_> {
    fn effective_level(&self, id: &str) -> RuleLevel {
        if let Some(explicit) = self.config.rules.get(id) {
            return *explicit;
        }
        let default = RULES.iter().find(|rule| rule.0 == id)
            .map_or(RuleLevel::Deny, |rule| rule.5);
        if self.config.preset == "strict"
            && (default == RuleLevel::Warn || matches!(id, "SCH024"))
        {
            RuleLevel::Deny
        } else {
            default
        }
    }

    fn emit(&mut self, id: &str, node: Option<&Node>, point: Option<Point>, message: String) {
        let level = self.effective_level(id);
        if level == RuleLevel::Allow {
            return;
        }
        let metadata = RULES.iter().find(|rule| rule.0 == id);
        let explanation = metadata.map(|rule| rule.4.to_owned());
        let severity = if level == RuleLevel::Deny { Severity::Error } else { Severity::Warning };
        let mut diagnostic = Diagnostic::new(id, severity, message);
        diagnostic.explanation = explanation;
        diagnostic.help = Some("Review the affected object in Eeschema; do not assume an automatic coordinate change preserves connectivity.".to_owned());
        diagnostic.source = Some(self.document.source().path().to_path_buf());
        diagnostic.span = node.and_then(span);
        diagnostic.schematic_location = point.map(|location| SchematicLocation {
            x_mm: location.x,
            y_mm: location.y,
        });
        self.diagnostics.push(diagnostic);
    }

    fn run_generic_rules(&mut self) {
        self.rule_wire_geometry();
        self.rule_label_topology();
        self.rule_label_wire_clearance();
        self.rule_text_boxes();
        self.rule_wire_obstacles();
        self.rule_label_collisions();
        self.rule_label_kinds();
        self.rule_duplicate_uuids();
        self.rule_symbol_grid(None);
    }

    fn run_profile_rules(&mut self, profile: &DesignProfile) {
        self.rule_direct_wiring(profile);
        self.rule_pin_orientation(profile);
        self.rule_component_spacing(profile);
        self.rule_group_metadata(profile);
        self.rule_group_containment(profile);
        self.rule_frame_padding(profile);
        self.rule_common_nets(profile);
        self.rule_no_connects(profile);
        self.rule_wire_budgets(profile);
        self.rule_circuit_documentation(profile);
        self.rule_multiline_annotations(Some(profile));
        self.rule_component_purposes(profile);
    }

    fn rule_wire_geometry(&mut self) {
        for (index, segment) in self.model.wires.iter().enumerate() {
            if segment.start == segment.end {
                self.emit("SCH001", None, Some(segment.start), format!("wire {index} has zero length"));
            } else if (segment.start.x - segment.end.x).abs() > TOL
                && (segment.start.y - segment.end.y).abs() > TOL
            {
                self.emit("SCH001", None, Some(segment.start), format!("wire {index} is diagonal: {:?} -> {:?}", segment.start, segment.end));
            }
        }

        for first_index in 0..self.model.wires.len() {
            for second_index in first_index + 1..self.model.wires.len() {
                let first = self.model.wires[first_index];
                let second = self.model.wires[second_index];
                let first_horizontal = (first.start.y - first.end.y).abs() <= TOL;
                let second_horizontal = (second.start.y - second.end.y).abs() <= TOL;
                if first_horizontal == second_horizontal {
                    let same_line = if first_horizontal {
                        (first.start.y - second.start.y).abs() <= TOL
                    } else {
                        (first.start.x - second.start.x).abs() <= TOL
                    };
                    if same_line && overlap_length(first, second, first_horizontal) > TOL {
                        self.emit("SCH001", None, Some(first.start), format!("collinear wire overlap: {:?}->{:?} and {:?}->{:?}", first.start, first.end, second.start, second.end));
                    }
                    continue;
                }

                let (horizontal, vertical) = if first_horizontal { (first, second) } else { (second, first) };
                let cross = Point::new(vertical.start.x, horizontal.start.y);
                if !point_on_segment(cross, horizontal.start, horizontal.end)
                    || !point_on_segment(cross, vertical.start, vertical.end)
                {
                    continue;
                }
                let horizontal_end = cross == horizontal.start || cross == horizontal.end;
                let vertical_end = cross == vertical.start || cross == vertical.end;
                if !horizontal_end && !vertical_end {
                    self.emit("SCH001", None, Some(cross), format!("wire crossing at ({:.3}, {:.3}) has no branch junction", cross.x, cross.y));
                } else if !(horizontal_end && vertical_end)
                    && !self.model.junctions.contains(&cross)
                {
                    self.emit("SCH001", None, Some(cross), format!("T-junction at ({:.3}, {:.3}) must terminate at a wire endpoint or explicit junction", cross.x, cross.y));
                }
            }
        }
    }

    fn rule_label_topology(&mut self) {
        let network = WireNetwork::new(self.model);
        let mut totals: BTreeMap<String, usize> = BTreeMap::new();
        let mut islands: BTreeMap<(String, usize), usize> = BTreeMap::new();
        for label in self.model.labels.iter().filter(|label| label.kind == "label") {
            let Some(root) = network.root_for(label.point) else {
                self.emit("SCH002", Some(label.node), Some(label.point), format!("local label {:?} is not attached to a wire", label.name));
                continue;
            };
            *totals.entry(label.name.clone()).or_default() += 1;
            *islands.entry((label.name.clone(), root)).or_default() += 1;
        }
        for (name, count) in totals {
            if count < 2 {
                if let Some(label) = self.model.labels.iter().find(|item| item.kind == "label" && item.name == name) {
                    self.emit("SCH002", Some(label.node), Some(label.point), format!("local label {name:?} is used only {count} time(s); use it to connect distinct wire islands"));
                }
            }
        }
        for ((name, _), count) in islands {
            if count > 1 {
                if let Some(label) = self.model.labels.iter().find(|item| item.kind == "label" && item.name == name) {
                    self.emit("SCH002", Some(label.node), Some(label.point), format!("local label {name:?} occurs {count} times on one physical wire network"));
                }
            }
        }

        for label in self.model.labels.iter().filter(|label| label.kind == "hierarchical_label") {
            let degree = self.model.wires.iter().filter(|wire| {
                wire.start == label.point || wire.end == label.point
            }).count();
            let attached = self.model.wires.iter().any(|wire| point_on_segment(label.point, wire.start, wire.end));
            if degree != 1 || !attached {
                self.emit("SCH002", Some(label.node), Some(label.point), format!("hierarchical label {:?} must terminate a single wire twig (found endpoint degree {degree})", label.name));
            }
        }
    }

    fn rule_label_wire_clearance(&mut self) {
        for label in &self.model.labels {
            if self.model.wires.iter().any(|wire| line_hits_rect(wire.start, wire.end, label.bounds, true)) {
                self.emit("SCH003", Some(label.node), Some(label.point), format!("{} label {:?} overlaps a wire segment", label.kind, label.name));
            }
        }
    }

    fn rule_text_boxes(&mut self) {
        let root = &self.document.cst().nodes[0];
        for text in direct_nodes(root, "text") {
            self.emit("SCH008", Some(text), None, "free-standing annotation text is forbidden; use a native text_box".to_owned());
        }
        for item in &self.model.text_boxes {
            let margins = direct_nodes(item.node, "margins").first().copied();
            let padding: Vec<f64> = margins.map_or_else(Vec::new, |node| {
                (1..5).filter_map(|index| atom_at(node, index).and_then(|value| value.parse().ok())).collect()
            });
            if padding.len() != 4 || padding.iter().any(|value| (*value - 1.27).abs() > TOL) {
                self.emit("SCH008", Some(item.node), Some(item.bounds.min), "text_box must use four equal 1.27 mm margins".to_owned());
            }
            let effects = direct_nodes(item.node, "effects").first().copied();
            let font = effects.and_then(|effects| direct_nodes(effects, "font").first().copied())
                .and_then(|font| direct_nodes(font, "size").first().copied())
                .and_then(|size| atom_at(size, 1))
                .and_then(|value| value.parse::<f64>().ok());
            if font.is_none() {
                self.emit("SCH008", Some(item.node), Some(item.bounds.min), "text_box must define a font size".to_owned());
                continue;
            }
            let font = font.unwrap_or(1.0);
            let required = required_text_size(&item.text, font, 1.27);
            if item.bounds.width() + TOL < required.0 || item.bounds.height() + TOL < required.1 {
                self.emit("SCH008", Some(item.node), Some(item.bounds.min), format!("text_box is undersized ({:.2} × {:.2} mm); estimated minimum is {:.2} × {:.2} mm", item.bounds.width(), item.bounds.height(), required.0, required.1));
            }
            if !item.outlined && item.bounds.width() + TOL < required.0 + 10.16 {
                self.emit("SCH008", Some(item.node), Some(item.bounds.min), format!("unbordered title box needs a further 10.16 mm single-line reserve (estimated width {:.2} mm)", required.0 + 10.16));
            }
        }

        for index in 0..self.model.text_boxes.len() {
            for other in self.model.text_boxes.iter().skip(index + 1) {
                let current = &self.model.text_boxes[index];
                if current.bounds.overlaps(other.bounds) {
                    self.emit("SCH008", Some(current.node), Some(current.bounds.min), "text boxes overlap".to_owned());
                }
            }
            let current = &self.model.text_boxes[index];
            if self.model.wires.iter().any(|wire| line_hits_rect(wire.start, wire.end, current.bounds, false)) {
                self.emit("SCH008", Some(current.node), Some(current.bounds.min), "a wire penetrates this text box".to_owned());
            }
        }
    }

    fn rule_symbol_field_clearance(&mut self, profile: Option<&DesignProfile>) {
        let global_gap = profile.and_then(|profile| profile.field_symbol_max_gap_mm).unwrap_or(9.0);
        for symbol in &self.model.symbols {
            let body = self.model.bounds_for_symbol(&symbol.reference);
            for field in &symbol.fields {
                let visible = direct_nodes(field.node, "effects").first()
                    .is_none_or(|effects| direct_nodes(effects, "hide").is_empty());
                if !visible { continue; }
                let own_body = body;
                if let Some(body) = own_body {
                    if field.bounds.overlaps(body) {
                        let wholly_inside = body.contains_rect(field.bounds, 0.0);
                        if field.name == "Value" && wholly_inside { /* common passive-value placement */ }
                        else {
                            self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} {} its symbol body", symbol.reference, field.name, if wholly_inside { "is embedded inside" } else { "partially straddles" }));
                        }
                    } else {
                        let gap = body.gap(field.bounds);
                        let maximum = profile.and_then(|profile| profile.field_symbol_gap_overrides_mm.get(&symbol.reference).copied()).unwrap_or(global_gap);
                        if gap > maximum + TOL {
                            self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} is {:.2} mm from visible symbol geometry; maximum is {:.2} mm", symbol.reference, field.name, gap, maximum));
                        }
                    }
                }
                if self.model.wires.iter().any(|wire| line_hits_rect(wire.start, wire.end, field.bounds, true)) {
                    self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} overlaps a wire", symbol.reference, field.name));
                }
                if self.model.pins.iter().any(|pin| field.bounds.contains(pin.point, 0.0)) {
                    self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} overlaps a symbol pin", symbol.reference, field.name));
                }
                for label in &self.model.labels {
                    if field.bounds.overlaps(label.bounds) {
                        self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} overlaps label {:?}", symbol.reference, field.name, label.name));
                    }
                }
                for text in &self.model.text_boxes {
                    if field.bounds.overlaps(text.bounds) {
                        self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} overlaps annotation text", symbol.reference, field.name));
                    }
                }
                for (other_reference, other_body) in &self.model.primitives {
                    if other_reference != &symbol.reference
                        && self.model.bounds_for_symbol(other_reference).is_some_and(|rect| field.bounds.overlaps(rect))
                    {
                        let _ = other_body;
                        self.emit("SCH010", Some(field.node), Some(field.point), format!("{} {} overlaps symbol body {}", symbol.reference, field.name, other_reference));
                    }
                }
            }
            for index in 0..symbol.fields.len() {
                for other in symbol.fields.iter().skip(index + 1) {
                    if symbol.fields[index].bounds.overlaps(other.bounds) {
                        self.emit("SCH010", Some(symbol.fields[index].node), Some(symbol.fields[index].point), format!("{} {} overlaps its {} field", symbol.reference, symbol.fields[index].name, other.name));
                    }
                }
            }
        }
    }

    fn rule_multiline_annotations(&mut self, profile: Option<&DesignProfile>) {
        for item in &self.model.text_boxes {
            if (item.text.contains('\n') || item.text.contains('\r')) && !item.outlined {
                self.emit("SCH011", Some(item.node), Some(item.bounds.min), "multiline annotation must have its own visible box outline".to_owned());
            }
        }
        if let Some(profile) = profile {
            for group in &profile.groups {
                if let Some(item) = self.model.text_boxes.iter().find(|item| group.description_phrases.iter().any(|phrase| item.text.contains(phrase)))
                    && !item.outlined
                {
                    self.emit("SCH011", Some(item.node), Some(item.bounds.min), format!("group {} description must have its own visible outline", group.id));
                }
            }
        }
    }

    fn rule_wire_obstacles(&mut self) {
        for (index, wire) in self.model.wires.iter().enumerate() {
            for (reference, primitives) in &self.model.primitives {
                if primitives.iter().any(|primitive| {
                    primitive_intersects_segment(primitive, wire.start, wire.end)
                        && !self.model.pins.iter().any(|pin| pin.reference == *reference
                            && (pin.point == wire.start || pin.point == wire.end))
                }) {
                    self.emit("SCH013", None, Some(wire.start), format!("wire {index} penetrates actual symbol outline {reference}"));
                }
            }
            for pin in &self.model.pins {
                if pin.point != wire.start && pin.point != wire.end
                    && point_on_segment(pin.point, wire.start, wire.end)
                {
                    self.emit("SCH013", None, Some(pin.point), format!("wire {index} passes through intermediate pin {}.{}", pin.reference, pin.number));
                }
            }
            for symbol in &self.model.symbols {
                for field in &symbol.fields {
                    if line_hits_rect(wire.start, wire.end, field.bounds, true) {
                        self.emit("SCH013", Some(field.node), Some(field.point), format!("wire {index} passes through {} {} field", symbol.reference, field.name));
                    }
                }
            }
            for text in &self.model.text_boxes {
                if line_hits_rect(wire.start, wire.end, text.bounds, true) {
                    self.emit("SCH013", Some(text.node), Some(text.bounds.min), format!("wire {index} penetrates an annotation text box"));
                }
            }
        }
    }

    fn rule_label_collisions(&mut self) {
        for index in 0..self.model.labels.len() {
            let current = &self.model.labels[index];
            for other in self.model.labels.iter().skip(index + 1) {
                if current.point != other.point && current.bounds.overlaps(other.bounds) {
                    self.emit("SCH018", Some(current.node), Some(current.point), format!("{} label {:?} overlaps {} label {:?}", current.kind, current.name, other.kind, other.name));
                }
            }
            for symbol in &self.model.symbols {
                for field in &symbol.fields {
                    if current.bounds.overlaps(field.bounds) {
                        self.emit("SCH018", Some(current.node), Some(current.point), format!("label {:?} overlaps {} {}", current.name, symbol.reference, field.name));
                    }
                }
            }
            for text in &self.model.text_boxes {
                if current.bounds.overlaps(text.bounds) {
                    self.emit("SCH018", Some(current.node), Some(current.point), format!("label {:?} overlaps a text box", current.name));
                }
            }
        }
    }

    fn rule_label_kinds(&mut self) {
        let mut kinds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for label in &self.model.labels {
            kinds.entry(label.name.clone()).or_default().insert(label.kind.to_owned());
        }
        for (name, values) in kinds.into_iter().filter(|(_, values)| values.len() > 1) {
            if let Some(label) = self.model.labels.iter().find(|label| label.name == name) {
                self.emit("SCH019", Some(label.node), Some(label.point), format!("label {name:?} mixes local, hierarchical or global label types"));
            }
        }
    }

    fn rule_direct_wiring(&mut self, profile: &DesignProfile) {
        let network = WireNetwork::new(self.model);
        for pair in &profile.direct_wire_pairs {
            self.check_direct_pair(pair, &network);
        }
    }

    fn check_direct_pair(&mut self, pair: &PinPair, network: &WireNetwork) {
        let first = parse_pin_reference(&pair.first);
        let second = parse_pin_reference(&pair.second);
        let (Some((ref_a, pin_a)), Some((ref_b, pin_b))) = (first, second) else {
            self.emit("SCH004", None, None, format!("invalid direct-wire pair {:?}/{:?}; expected REFERENCE.NUMBER", pair.first, pair.second));
            return;
        };
        let (Some(a), Some(b)) = (self.model.pin(ref_a, pin_a), self.model.pin(ref_b, pin_b)) else {
            self.emit("SCH004", None, None, format!("required physical net is missing pin {} or {}", pair.first, pair.second));
            return;
        };
        if network.root_for(a.point) != network.root_for(b.point) {
            self.emit("SCH004", None, Some(a.point), format!("{}.{} must be physically wire-connected to {}.{}", ref_a, pin_a, ref_b, pin_b));
        }
        if self.model.labels.iter().any(|label| label.point == a.point || label.point == b.point) {
            self.emit("SCH004", None, Some(a.point), format!("{}.{} / {}.{} must use a direct wire, not labels at the pin endpoints", ref_a, pin_a, ref_b, pin_b));
        }
    }

    fn rule_pin_orientation(&mut self, profile: &DesignProfile) {
        for id in &profile.ground_pins_down {
            let Some((reference, number)) = parse_pin_reference(id) else { continue; };
            match self.model.pin(reference, number) {
                Some(pin) if pin.angle.rem_euclid(360) != 90 => {
                    self.emit("SCH005", None, Some(pin.point), format!("ground pin {id} points at {} degrees; expected 90 degrees (down)", pin.angle));
                }
                None => self.emit("SCH005", None, None, format!("ground orientation contract references missing pin {id}")),
                _ => {}
            }
        }
        for label in self.model.labels.iter().filter(|label| label.kind == "hierarchical_label" && label.name == "GND") {
            let downward_wire = self.model.wires.iter().any(|wire| {
                (wire.start == label.point && wire.end.y < label.point.y - TOL)
                    || (wire.end == label.point && wire.start.y < label.point.y - TOL)
            });
            if downward_wire && label.angle.rem_euclid(360) != 90 {
                self.emit("SCH005", Some(label.node), Some(label.point), "downward hierarchical GND port must be oriented at 90 degrees".to_owned());
            }
        }
    }

    fn rule_component_spacing(&mut self, profile: &DesignProfile) {
        for pair in &profile.adjacent_components {
            self.check_adjacency(pair);
        }
        for symbol in &self.model.symbols {
            let Some(limit) = profile.property_distance_limits_mm.get("resistor_reference")
                .filter(|_| symbol.reference.starts_with('R'))
                .or_else(|| profile.property_distance_limits_mm.get("capacitor_reference")
                    .filter(|_| symbol.reference.starts_with('C')))
            else { continue; };
            for field in &symbol.fields {
                let maximum = match field.name.as_str() {
                    "Reference" => *limit,
                    "Value" if symbol.reference.starts_with('C') => profile.property_distance_limits_mm.get("capacitor_value").copied().unwrap_or(5.5),
                    _ => continue,
                };
                let distance = symbol.point.distance(field.point);
                if distance > maximum + TOL {
                    self.emit("SCH006", Some(field.node), Some(field.point), format!("{} {} field is {:.2} mm from the symbol origin; maximum is {:.2} mm", symbol.reference, field.name, distance, maximum));
                }
            }
        }
    }

    fn check_adjacency(&mut self, pair: &AdjacencyContract) {
        let (Some(left), Some(right)) = (self.model.symbol(&pair.left), self.model.symbol(&pair.right)) else {
            self.emit("SCH006", None, None, format!("adjacency contract references missing components {}/{}", pair.left, pair.right));
            return;
        };
        if (left.point.y - right.point.y).abs() > pair.alignment_tolerance_mm {
            self.emit("SCH006", Some(right.node), Some(right.point), format!("adjacent components {}/{} are not aligned", pair.left, pair.right));
            return;
        }
        let gap = right.point.x - left.point.x - pair.body_width_mm;
        if gap > pair.max_gap_mm + TOL {
            self.emit("SCH006", Some(right.node), Some(right.point), format!("components {}/{} have a {:.2} mm gap; maximum is {:.2} mm", pair.left, pair.right, gap, pair.max_gap_mm));
        }
    }

    fn rule_group_metadata(&mut self, profile: &DesignProfile) {
        for group in &profile.groups {
            if self.model.text_boxes.iter().all(|item| item.text != group.title) {
                self.emit("SCH007", None, None, format!("functional group {} is missing title {:?}", group.id, group.title));
            }
            if self.model.node_for_group_description(&group.description_phrases).is_none() {
                self.emit("SCH007", None, None, format!("functional group {} is missing its description panel", group.id));
            }
            if self.model.frame_for_group(&group.title).is_none() {
                self.emit("SCH007", None, None, format!("functional group {} is missing a frame around its title", group.id));
            }
        }
    }

    fn rule_group_containment(&mut self, profile: &DesignProfile) {
        let mut group_frames: Vec<(&DesignGroup, Rect)> = Vec::new();
        for group in &profile.groups {
            if let Some(frame) = self.model.frame_for_group(&group.title) {
                group_frames.push((group, frame));
                for reference in &group.members {
                    let Some(symbol) = self.model.symbol(reference) else {
                        self.emit("SCH009", None, None, format!("group {} references missing component {reference}", group.id));
                        continue;
                    };
                    if !frame.contains(symbol.point, 2.54) {
                        self.emit("SCH009", Some(symbol.node), Some(symbol.point), format!("component {reference} is not comfortably inside frame {}", group.id));
                    }
                    for pin in self.model.pins.iter().filter(|pin| pin.reference == *reference) {
                        if !frame.contains(pin.point, 0.0) {
                            self.emit("SCH009", Some(symbol.node), Some(pin.point), format!("pin {reference}.{} crosses outside frame {}", pin.number, group.id));
                        }
                    }
                    for field in &symbol.fields {
                        if !frame.contains_rect(field.bounds, 0.0) {
                            self.emit("SCH009", Some(field.node), Some(field.point), format!("{reference} {} text crosses outside frame {}", field.name, group.id));
                        }
                    }
                }
                for label in &self.model.labels {
                    if frame.contains(label.point, 0.0) && !frame.contains_rect(label.bounds, 0.0) {
                        self.emit("SCH009", Some(label.node), Some(label.point), format!("{} label {:?} extends outside its owning group frame {}", label.kind, label.name, group.id));
                    }
                }
                for text in &self.model.text_boxes {
                    if text.text != group.title && frame.overlaps(text.bounds) && !frame.contains_rect(text.bounds, 0.0) {
                        self.emit("SCH009", Some(text.node), Some(text.bounds.min), format!("text box penetrates group frame {}", group.id));
                    }
                }
                for wire in &self.model.wires {
                    let start_inside = frame.contains(wire.start, 0.0);
                    let end_inside = frame.contains(wire.end, 0.0);
                    if !(start_inside && end_inside) && line_hits_rect(wire.start, wire.end, frame, true) {
                        self.emit("SCH009", None, Some(wire.start), format!("wire crosses functional frame {} boundary", group.id));
                    }
                }
            }
        }
        for index in 0..group_frames.len() {
            for (other, other_rect) in group_frames.iter().skip(index + 1) {
                let (group, rect) = group_frames[index];
                if rect.overlaps(*other_rect) {
                    self.emit("SCH009", None, Some(rect.min), format!("functional frames {} and {} overlap", group.id, other.id));
                }
            }
        }
        let mut primary: Vec<_> = group_frames.iter().filter(|(group, _)| group.primary).collect();
        if let Some(expected_gap) = profile.group_gap_mm {
            for pair in primary.windows(2) {
                let gap = pair[1].1.min.x - pair[0].1.max.x;
                if (gap - expected_gap).abs() > TOL {
                    self.emit("SCH009", None, Some(pair[1].1.min), format!("primary frame gap {} -> {} is {:.2} mm; expected {:.2} mm", pair[0].0.id, pair[1].0.id, gap, expected_gap));
                }
            }
        }
        for symbol in &self.model.symbols {
            let approved_dnp = profile.dnp_components.get(&symbol.reference);
            if let Some(expected_value) = approved_dnp {
                if !symbol.dnp || symbol.value != *expected_value {
                    self.emit("SCH009", Some(symbol.node), Some(symbol.point), format!("{} must have Value={} and DNP=yes", symbol.reference, expected_value));
                }
            } else if symbol.dnp {
                self.emit("SCH009", Some(symbol.node), Some(symbol.point), format!("{} is DNP but is not an approved optional-population component", symbol.reference));
            }
        }
    }

    fn rule_frame_padding(&mut self, profile: &DesignProfile) {
        let margin = profile.frame_content_padding_mm.unwrap_or(2.54);
        for group in &profile.groups {
            let Some(frame) = self.model.frame_for_group(&group.title) else { continue; };
            for label in self.model.labels.iter().filter(|label| frame.contains(label.point, 0.0)) {
                if !frame.contains_rect(label.bounds, margin) {
                    self.emit("SCH012", Some(label.node), Some(label.point), format!("{} label {:?} is closer than {:.2} mm to frame {}", label.kind, label.name, margin, group.id));
                }
            }
            for text in self.model.text_boxes.iter().filter(|text| group.description_phrases.iter().any(|phrase| text.text.contains(phrase))) {
                if !frame.contains_rect(text.bounds, margin) {
                    self.emit("SCH012", Some(text.node), Some(text.bounds.min), format!("group {} description panel violates {:.2} mm frame padding", group.id, margin));
                }
            }
        }
    }

    fn rule_common_nets(&mut self, profile: &DesignProfile) {
        let network = WireNetwork::new(self.model);
        for contract in &profile.common_nets {
            self.check_common_net(contract, &network);
        }
    }

    fn check_common_net(&mut self, contract: &CommonNetContract, network: &WireNetwork) {
        let mut roots = BTreeSet::new();
        for pin_id in &contract.pins {
            let Some((reference, number)) = parse_pin_reference(pin_id) else { continue; };
            let Some(pin) = self.model.pin(reference, number) else {
                self.emit("SCH014", None, None, format!("common net {} references missing pin {pin_id}", contract.name));
                continue;
            };
            let root = network.root_for(pin.point);
            if !self.model.labels.iter().any(|label| label.name == contract.name && network.root_for(label.point) == root) {
                self.emit("SCH014", None, Some(pin.point), format!("pin {pin_id} must belong to common net {}", contract.name));
            }
            if let Some(root) = root { roots.insert(root); }
        }
        if contract.wire_connected && roots.len() > 1 {
            self.emit("SCH014", None, None, format!("configured pins {:?} must share one physical {} wire network", contract.pins, contract.name));
        }
        if contract.single_label {
            let count = self.model.labels.iter().filter(|label| label.name == contract.name && network.root_for(label.point).is_some_and(|root| roots.contains(&root))).count();
            if count != 1 {
                self.emit("SCH014", None, None, format!("configured pins {:?} must share exactly one {} label; found {count}", contract.pins, contract.name));
            }
        }
    }

    fn rule_no_connects(&mut self, profile: &DesignProfile) {
        for pin_id in &profile.required_no_connect_pins {
            let Some((reference, number)) = parse_pin_reference(pin_id) else { continue; };
            match self.model.pin(reference, number) {
                None => self.emit("SCH015", None, None, format!("required no-connect contract references missing pin {pin_id}")),
                Some(pin) if !self.model.no_connects.contains(&pin.point) => {
                    self.emit("SCH015", None, Some(pin.point), format!("unused pin {pin_id} requires an explicit no-connect marker"));
                }
                _ => {}
            }
        }
    }

    fn rule_wire_budgets(&mut self, profile: &DesignProfile) {
        for (group_id, budget) in &profile.wire_length_budgets_mm {
            let Some(group) = profile.groups.iter().find(|group| group.id == *group_id) else {
                self.emit("SCH016", None, None, format!("wire budget references unknown group {group_id}"));
                continue;
            };
            let Some(frame) = self.model.frame_for_group(&group.title) else {
                self.emit("SCH016", None, None, format!("wire budget references missing frame for {group_id}"));
                continue;
            };
            if *budget <= 0.0 {
                self.emit("SCH016", None, None, format!("wire budget for {group_id} must be positive"));
                continue;
            }
            let total: f64 = self.model.wires.iter().filter(|wire| frame.contains(wire.start, 0.0) && frame.contains(wire.end, 0.0))
                .map(|wire| wire.start.distance(wire.end)).sum();
            let score = total / budget;
            if score > 1.0 + TOL {
                self.emit("SCH016", None, Some(frame.min), format!("group {group_id} wire compactness score is {score:.3} ({total:.2}/{budget:.2} mm); maximum is 1.000"));
            }
        }
    }

    fn rule_circuit_documentation(&mut self, profile: &DesignProfile) {
        let evidence = &profile.evidence;
        let document_reference = evidence.document.as_ref().is_some_and(|value| !value.trim().is_empty())
            && evidence.page.as_ref().is_some_and(|value| !value.trim().is_empty())
            && evidence.figure.as_ref().is_some_and(|value| !value.trim().is_empty());
        let reviewed_asset = evidence.screenshot_asset.as_ref().is_some_and(|value| !value.trim().is_empty());
        if !document_reference && !reviewed_asset {
            self.emit("SCH017", None, None, "profile evidence must identify document/page/figure or a reviewed screenshot asset".to_owned());
        }
        for group in &profile.groups {
            if group.documentation_members.is_empty() { continue; }
            let description = self.model.node_for_group_description(&group.description_phrases)
                .and_then(|node| atom_at(node, 1)).unwrap_or_default();
            let missing: Vec<_> = group.documentation_members.iter()
                .filter(|reference| !description.contains(reference.as_str()))
                .cloned().collect();
            if !missing.is_empty() {
                self.emit("SCH017", None, None, format!("group {} description must identify documented members: {}", group.id, missing.join(", ")));
            }
        }
    }

    fn rule_component_purposes(&mut self, profile: &DesignProfile) {
        for group in &profile.groups {
            let description = self.model.node_for_group_description(&group.description_phrases)
                .and_then(|node| atom_at(node, 1)).unwrap_or_default().to_owned();
            for purpose in &group.component_purposes {
                if !group.members.contains(&purpose.reference) {
                    self.emit("SCH020", None, None, format!("group {} documents {}, which is not a group member", group.id, purpose.reference));
                }
                if !group.documentation_members.contains(&purpose.reference) {
                    self.emit("SCH020", None, None, format!("group {} must list {} as a documentation member", group.id, purpose.reference));
                }
                for (field, phrase) in [("function", &purpose.function), ("rationale", &purpose.rationale)] {
                    if phrase.trim().is_empty() || !description.contains(phrase) {
                        self.emit("SCH020", None, None, format!("group {} description must include {} {} ({})", group.id, purpose.reference, field, phrase));
                    }
                }
            }
        }
    }

    fn rule_component_metadata(&mut self, profile: Option<&DesignProfile>) {
        let metadata = profile.map_or(&[][..], |profile| profile.component_metadata.as_slice());
        let mut by_reference = BTreeMap::new();
        for item in metadata {
            if by_reference.insert(item.reference.as_str(), item).is_some() {
                self.emit("SCH024", None, None, format!("component metadata contains duplicate reference {}", item.reference));
            }
            let Some(symbol) = self.model.symbol(&item.reference) else {
                self.emit("SCH024", None, None, format!("component metadata references missing symbol {}", item.reference));
                continue;
            };
            if !symbol_matches_kind(symbol, item.kind) {
                self.emit("SCH024", Some(symbol.node), Some(symbol.point), format!("{} is declared as {:?}, but its reference/library symbol does not match that component type", item.reference, item.kind));
            }
            if item.function.trim().is_empty() || item.rationale.trim().is_empty() {
                self.emit("SCH024", Some(symbol.node), Some(symbol.point), format!("{} metadata must provide both function and rationale", item.reference));
            }
            if !role_matches_kind(item.kind, item.role) {
                self.emit("SCH024", Some(symbol.node), Some(symbol.point), format!("role {:?} is not valid for component type {:?}", item.role, item.kind));
            }
            if item.kind == ComponentKind::Capacitor && item.role == ComponentRole::Decoupling {
                for (field, value) in [
                    ("associated_pin", item.associated_pin.as_deref()),
                    ("connection_pin", item.connection_pin.as_deref()),
                    ("return_pin", item.return_pin.as_deref()),
                    ("return_net", item.return_net.as_deref()),
                ] {
                    if value.is_none_or(str::is_empty) {
                        self.emit("SCH024", Some(symbol.node), Some(symbol.point), format!("decoupling capacitor {} must declare {field}", item.reference));
                    }
                }
            }
        }

        let mut required = profile.map_or_else(Vec::new, |profile| profile.require_component_metadata_for.clone());
        if self.config.preset == "strict" && !required.contains(&ComponentKind::Capacitor) {
            required.push(ComponentKind::Capacitor);
        }
        required.sort();
        required.dedup();
        for symbol in &self.model.symbols {
            for kind in &required {
                if symbol_matches_kind(symbol, *kind)
                    && !by_reference.contains_key(symbol.reference.as_str())
                {
                    self.emit("SCH024", Some(symbol.node), Some(symbol.point), format!("{} is a {:?} but has no declared component-intent metadata", symbol.reference, kind));
                }
            }
        }
    }

    fn rule_decoupling_networks(&mut self, profile: Option<&DesignProfile>) {
        let Some(profile) = profile else { return; };
        let decouplers: Vec<&ComponentMetadata> = profile.component_metadata.iter()
            .filter(|item| item.kind == ComponentKind::Capacitor && item.role == ComponentRole::Decoupling)
            .collect();
        if decouplers.is_empty() { return; }

        let network = WireNetwork::new(self.model);
        let mut clusters: BTreeMap<String, Vec<&ComponentMetadata>> = BTreeMap::new();
        for item in &decouplers {
            let target = item.associated_pin.as_deref().unwrap_or_default();
            clusters.entry(target.to_owned()).or_default().push(item);

            let Some(symbol) = self.model.symbol(&item.reference) else { continue; };
            let (Some(target_id), Some(connection_number), Some(return_number), Some(return_net)) = (
                item.associated_pin.as_deref(),
                item.connection_pin.as_deref(),
                item.return_pin.as_deref(),
                item.return_net.as_deref(),
            ) else { continue; };
            let Some((target_ref, target_pin_number)) = parse_pin_reference(target_id) else {
                self.emit("SCH025", Some(symbol.node), Some(symbol.point), format!("{} has invalid associated pin {:?}; expected REFERENCE.NUMBER", item.reference, target_id));
                continue;
            };
            let (Some(target_pin), Some(connection_pin), Some(return_pin)) = (
                self.model.pin(target_ref, target_pin_number),
                self.model.pin(&item.reference, connection_number),
                self.model.pin(&item.reference, return_number),
            ) else {
                self.emit("SCH025", Some(symbol.node), Some(symbol.point), format!("{} decoupling metadata references a missing target or capacitor terminal pin", item.reference));
                continue;
            };

            if !network.physically_connected(self.model, target_pin.point, connection_pin.point) {
                self.emit("SCH025", Some(symbol.node), Some(connection_pin.point), format!("{} terminal {} is not physically wire-connected to target {}", item.reference, connection_number, target_id));
            }
            let maximum_distance = item.max_distance_mm
                .or(profile.decoupling_max_pin_distance_mm)
                .unwrap_or(50.8);
            let distance = target_pin.point.distance(connection_pin.point);
            if distance > maximum_distance + TOL {
                self.emit("SCH025", Some(symbol.node), Some(connection_pin.point), format!("{} decoupling terminal is {:.2} mm from target {}; maximum is {:.2} mm", item.reference, distance, target_id, maximum_distance));
            }

            if !self.model.is_wire_attached(return_pin.point) {
                self.emit("SCH025", Some(symbol.node), Some(return_pin.point), format!("{} return terminal {} is not attached to a physical wire", item.reference, return_number));
                continue;
            }
            let return_root = network.root_for(return_pin.point);
            let has_return_label = self.model.labels.iter().any(|label| {
                label.name == return_net && network.root_for(label.point) == return_root
            });
            if !has_return_label {
                self.emit("SCH025", Some(symbol.node), Some(return_pin.point), format!("{} return terminal {} must be on the declared {} net", item.reference, return_number, return_net));
            }
        }

        let maximum_spacing = profile.decoupling_max_cluster_spacing_mm.unwrap_or(25.4);
        for (target, items) in clusters {
            let mut returns = Vec::new();
            let mut return_nets = BTreeSet::new();
            let symbols: Vec<_> = items.iter().filter_map(|item| {
                let symbol = self.model.symbol(&item.reference)?;
                let return_number = item.return_pin.as_deref()?;
                let return_pin = self.model.pin(&item.reference, return_number)?;
                if !self.model.is_wire_attached(return_pin.point) { return None; }
                let return_root = network.root_for(return_pin.point)?;
                return_nets.insert(item.return_net.clone().unwrap_or_default());
                returns.push((item, return_pin.point, return_root));
                Some((item, symbol))
            }).collect();

            for index in 0..symbols.len() {
                for (other_metadata, other_symbol) in symbols.iter().skip(index + 1) {
                    let (item, symbol) = symbols[index];
                    let spacing = symbol.point.distance(other_symbol.point);
                    if spacing > maximum_spacing + TOL {
                        self.emit("SCH025", Some(symbol.node), Some(symbol.point), format!("decouplers {} and {} for {} are {:.2} mm apart; maximum cluster spacing is {:.2} mm", item.reference, other_metadata.reference, target, spacing, maximum_spacing));
                    }
                }
            }

            if returns.len() > 1 {
                if return_nets.len() != 1 {
                    self.emit("SCH025", None, None, format!("decouplers for {target} declare inconsistent return nets: {return_nets:?}"));
                }
                let first_root = returns[0].2;
                for (item, point, root) in returns.iter().skip(1) {
                    if *root != first_root {
                        self.emit("SCH025", None, Some(*point), format!("decoupler {} does not share one physical return/GND network with the other capacitors for {target}", item.reference));
                    }
                }
            }
        }
    }

    fn rule_symbol_grid(&mut self, grid: Option<f64>) {
        let Some(grid) = grid.filter(|grid| *grid > 0.0) else { return; };
        for symbol in &self.model.symbols {
            if !is_on_grid(symbol.point.x, grid) || !is_on_grid(symbol.point.y, grid) {
                self.emit("SCH021", Some(symbol.node), Some(symbol.point), format!("symbol {} origin ({:.3}, {:.3}) is not aligned to the {:.3} mm design grid", symbol.reference, symbol.point.x, symbol.point.y, grid));
            }
        }
    }

    fn rule_duplicate_uuids(&mut self) {
        let mut seen = BTreeSet::new();
        for (uuid, location) in &self.model.uuids {
            if !seen.insert(uuid) {
                self.emit("SCH022", None, None, format!("UUID {uuid} occurs more than once in this schematic"));
                let _ = location;
            }
        }
    }

    fn rule_title_block(&mut self, profile: Option<&DesignProfile>) {
        let root = &self.document.cst().nodes[0];
        let Some(block) = direct_nodes(root, "title_block").first().copied() else {
            self.emit("SCH023", None, None, "schematic has no native title block".to_owned());
            return;
        };
        let required = profile.map_or_else(|| vec!["title".to_owned()], |profile| profile.required_title_fields.clone());
        for field in required {
            if self.model.title_fields.get(&field).is_none_or(|value| value.trim().is_empty()) {
                self.emit("SCH023", Some(block), None, format!("title block field {field:?} is empty or missing"));
            }
        }
    }
}

fn symbol_matches_kind(symbol: &crate::model::Symbol<'_>, kind: ComponentKind) -> bool {
    let reference = symbol.reference.to_ascii_uppercase();
    let library = symbol.lib_id.to_ascii_lowercase();
    match kind {
        ComponentKind::Capacitor => reference.starts_with('C') || library.contains(":c") || library.contains("capacitor"),
        ComponentKind::Resistor => reference.starts_with('R') || library.contains(":r") || library.contains("resistor"),
        ComponentKind::Diode => reference.starts_with('D') || library.contains("diode") || library.contains(":d"),
        ComponentKind::Inductor => reference.starts_with('L') || library.contains("inductor") || library.contains("ferrite"),
        ComponentKind::Transistor => reference.starts_with('Q') || reference.starts_with('T') || library.contains("transistor") || library.contains("mosfet"),
        ComponentKind::IntegratedCircuit => reference.starts_with('U') || library.contains("interface:") || library.contains("driver:"),
        ComponentKind::Connector => reference.starts_with('J') || library.contains("connector:") || library.contains("terminalblock:"),
        ComponentKind::TestPoint => reference.starts_with("TP") || library.contains("testpoint"),
        ComponentKind::Other => true,
    }
}

fn role_matches_kind(kind: ComponentKind, role: ComponentRole) -> bool {
    use ComponentKind as Kind;
    use ComponentRole as Role;
    match kind {
        Kind::Capacitor => matches!(role, Role::Bulk | Role::Decoupling | Role::Bypass | Role::Filter | Role::Coupling | Role::Snubber | Role::Reservoir | Role::Other),
        Kind::Resistor => matches!(role, Role::PullUp | Role::PullDown | Role::SeriesCurrentLimit | Role::Discharge | Role::GateBias | Role::Bleeder | Role::CurrentSense | Role::Snubber | Role::Other),
        Kind::Diode => matches!(role, Role::Clamp | Role::EsdProtection | Role::Flyback | Role::Rectifier | Role::ReversePolarity | Role::Other),
        Kind::Inductor => matches!(role, Role::Filter | Role::Snubber | Role::Other),
        Kind::Transistor => matches!(role, Role::HighSideSwitch | Role::LowSideSwitch | Role::BackToBackSwitch | Role::GateBias | Role::Other),
        Kind::IntegratedCircuit => matches!(role, Role::Interface | Role::Other),
        Kind::Connector => matches!(role, Role::Interface | Role::TestAccess | Role::Other),
        Kind::TestPoint => matches!(role, Role::TestAccess | Role::Other),
        Kind::Other => true,
    }
}

fn symbol_matches_kind(symbol: &crate::model::Symbol<'_>, kind: ComponentKind) -> bool {
    let reference = symbol.reference.to_ascii_uppercase();
    let library = symbol.lib_id.to_ascii_lowercase();
    match kind {
        ComponentKind::Capacitor => reference.starts_with('C') || library.contains(":c") || library.contains("capacitor"),
        ComponentKind::Resistor => reference.starts_with('R') || library.contains(":r") || library.contains("resistor"),
        ComponentKind::Diode => reference.starts_with('D') || library.contains("diode") || library.contains(":d"),
        ComponentKind::Inductor => reference.starts_with('L') || library.contains("inductor") || library.contains("ferrite"),
        ComponentKind::Transistor => reference.starts_with('Q') || reference.starts_with('T') || library.contains("transistor") || library.contains("mosfet"),
        ComponentKind::IntegratedCircuit => reference.starts_with('U') || library.contains("interface:") || library.contains("driver:"),
        ComponentKind::Connector => reference.starts_with('J') || library.contains("connector:") || library.contains("terminalblock:"),
        ComponentKind::TestPoint => reference.starts_with("TP") || library.contains("testpoint"),
        ComponentKind::Other => true,
    }
}

fn role_matches_kind(kind: ComponentKind, role: ComponentRole) -> bool {
    use ComponentKind as Kind;
    use ComponentRole as Role;
    match kind {
        Kind::Capacitor => matches!(role, Role::Bulk | Role::Decoupling | Role::Bypass | Role::Filter | Role::Coupling | Role::Snubber | Role::Reservoir | Role::Other),
        Kind::Resistor => matches!(role, Role::PullUp | Role::PullDown | Role::SeriesCurrentLimit | Role::Discharge | Role::GateBias | Role::Bleeder | Role::CurrentSense | Role::Snubber | Role::Other),
        Kind::Diode => matches!(role, Role::Clamp | Role::EsdProtection | Role::Flyback | Role::Rectifier | Role::ReversePolarity | Role::Other),
        Kind::Inductor => matches!(role, Role::Filter | Role::Snubber | Role::Other),
        Kind::Transistor => matches!(role, Role::HighSideSwitch | Role::LowSideSwitch | Role::BackToBackSwitch | Role::GateBias | Role::Other),
        Kind::IntegratedCircuit => matches!(role, Role::Interface | Role::Other),
        Kind::Connector => matches!(role, Role::Interface | Role::TestAccess | Role::Other),
        Kind::TestPoint => matches!(role, Role::TestAccess | Role::Other),
        Kind::Other => true,
    }
}

fn overlap_length(first: Segment, second: Segment, horizontal: bool) -> f64 {
    if horizontal {
        first.end.x.max(first.start.x).min(second.end.x.max(second.start.x))
            - first.end.x.min(first.start.x).max(second.end.x.min(second.start.x))
    } else {
        first.end.y.max(first.start.y).min(second.end.y.max(second.start.y))
            - first.end.y.min(first.start.y).max(second.end.y.min(second.start.y))
    }
}

fn required_text_size(value: &str, font: f64, padding: f64) -> (f64, f64) {
    let width = value.lines().map(|line| line.chars().map(|character| {
        if "ilI.,:;!|'".contains(character) { 0.30 }
        else if "MW@%&".contains(character) { 0.90 }
        else if character.is_whitespace() { 0.34 }
        else if character.is_uppercase() || character.is_numeric() { 0.64 }
        else { 0.55 }
    }).sum::<f64>()).fold(font, f64::max);
    (2.0 * padding + width * font * 1.30, 2.0 * padding + value.lines().count() as f64 * font * 1.55)
}

fn parse_pin_reference(value: &str) -> Option<(&str, &str)> {
    value.split_once('.').filter(|(reference, number)| !reference.is_empty() && !number.is_empty())
}

fn is_on_grid(value: f64, grid: f64) -> bool {
    let units = value / grid;
    (units - units.round()).abs() <= TOL
}

struct WireNetwork {
    roots: BTreeMap<(i64, i64), usize>,
}

impl WireNetwork {
    fn new(model: &SchematicModel<'_>) -> Self {
        let mut points: Vec<Point> = model.wires.iter().flat_map(|wire| [wire.start, wire.end]).collect();
        points.extend(model.labels.iter().map(|label| label.point));
        points.extend(model.pins.iter().map(|pin| pin.point));
        points.extend(model.junctions.iter().copied());
        points.sort_by(|a, b| point_key(*a).cmp(&point_key(*b)));
        points.dedup_by(|a, b| point_key(*a) == point_key(*b));
        let indexes: BTreeMap<_, _> = points.iter().enumerate().map(|(index, point)| (point_key(*point), index)).collect();
        let mut parent: Vec<usize> = (0..points.len()).collect();
        for wire in &model.wires {
            let mut split: Vec<_> = points.iter().copied().filter(|point| point_on_segment(*point, wire.start, wire.end)).collect();
            let horizontal = (wire.start.y - wire.end.y).abs() <= TOL;
            split.sort_by(|a, b| if horizontal { a.x.total_cmp(&b.x) } else { a.y.total_cmp(&b.y) });
            if (if horizontal { wire.start.x > wire.end.x } else { wire.start.y > wire.end.y }) { split.reverse(); }
            for pair in split.windows(2) {
                if let (Some(first), Some(second)) = (indexes.get(&point_key(pair[0])), indexes.get(&point_key(pair[1]))) {
                    union(&mut parent, *first, *second);
                }
            }
        }
        let roots = indexes.into_iter().map(|(point, index)| {
            let root = find(&mut parent, index);
            (point, root)
        }).collect();
        Self { roots }
    }

    fn root_for(&self, point: Point) -> Option<usize> {
        self.roots.get(&point_key(point)).copied()
    }

    fn physically_connected(&self, model: &SchematicModel<'_>, first: Point, second: Point) -> bool {
        model.is_wire_attached(first)
            && model.is_wire_attached(second)
            && self.root_for(first).is_some()
            && self.root_for(first) == self.root_for(second)
    }

    fn physically_connected(&self, model: &SchematicModel<'_>, first: Point, second: Point) -> bool {
        model.is_wire_attached(first)
            && model.is_wire_attached(second)
            && self.root_for(first).is_some()
            && self.root_for(first) == self.root_for(second)
    }
}

fn point_key(point: Point) -> (i64, i64) {
    ((point.x * 1000.0).round() as i64, (point.y * 1000.0).round() as i64)
}

fn find(parent: &mut [usize], value: usize) -> usize {
    if parent[value] != value {
        let root = find(parent, parent[value]);
        parent[value] = root;
    }
    parent[value]
}

fn union(parent: &mut [usize], first: usize, second: usize) {
    let root_a = find(parent, first);
    let root_b = find(parent, second);
    if root_a != root_b { parent[root_b] = root_a; }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use kicad_clippy_parser::SchematicDocument;

    use crate::{RuleConfig, run_rules};

    #[test]
    fn rule_catalogue_preserves_sch_ids_and_adds_research_rules() -> Result<(), Box<dyn Error>> {
        let registry = super::available_rules()?;
        for number in 1..=23 {
            assert!(registry.get(&format!("SCH{number:03}")).is_some());
        }
        assert_eq!(registry.len(), 23);
        Ok(())
    }

    #[test]
    fn generic_rules_run_without_a_project_profile() -> Result<(), Box<dyn Error>> {
        let source = "(kicad_sch (version 20250101) (generator eeschema) (uuid \"root\") (wire (pts (xy 0 0) (xy 5 5))))";
        let document = SchematicDocument::from_text("fixture.kicad_sch", source)?;
        let diagnostics = run_rules(&document, &RuleConfig::default())?;
        assert!(diagnostics.iter().any(|item| item.code == "SCH001"));
        assert!(!diagnostics.iter().any(|item| item.code == "SCH004"));
        Ok(())
    }

    #[test]
    fn duplicate_uuids_are_reported() -> Result<(), Box<dyn Error>> {
        let source = "(kicad_sch (version 20250101) (generator eeschema) (uuid \"duplicate\") (junction (at 1 1) (uuid \"duplicate\")))";
        let document = SchematicDocument::from_text("fixture.kicad_sch", source)?;
        let diagnostics = run_rules(&document, &RuleConfig::default())?;
        assert!(diagnostics.iter().any(|item| item.code == "SCH022"));
        Ok(())
    }

    #[test]
    fn explicit_allow_overrides_a_default_denial() -> Result<(), Box<dyn Error>> {
        let source = "(kicad_sch (version 20250101) (generator eeschema) (uuid \"root\") (wire (pts (xy 0 0) (xy 5 5))))";
        let document = SchematicDocument::from_text("fixture.kicad_sch", source)?;
        let mut config = RuleConfig::default();
        config.rules.insert("SCH001".to_owned(), kicad_clippy_core::RuleLevel::Allow);
        let diagnostics = run_rules(&document, &config)?;
        assert!(!diagnostics.iter().any(|item| item.code == "SCH001"));
        Ok(())
    }
}
