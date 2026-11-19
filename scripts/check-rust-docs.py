#!/usr/bin/env python3
"""Check every Rust function's doc comment, named parameters, and explicit return contract."""
from __future__ import annotations
import re
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "crates"

PLACEHOLDER_DOC_PATTERNS = (
    r"behaves as required",
    r"from the supplied schematic geometry or S-expression data",
    r"result derived from the schematic geometry",
    r"Calculates .* from the supplied schematic geometry",
    r"Verifies that .* behaves as required",
    r"using the supplied values and schematic data",
    r"Calculates (?:name|run) from the supplied schematic geometry or S-expression data",
    r"The name result derived from the schematic geometry",
    r"Input `[^`]+` used by this operation",
    r"Text value stored in the corresponding field",
    r"default level supplied by the caller",
    r"value to be used in this geometry or parsing operation",
    r"Input value to parse or convert",
    r"The value produced by this operation",
    r"The requested geometry or parsed value when the source data is sufficient",
    r"`true` when the geometry satisfies the condition checked by this helper",
    r"Diagnostics for each contract violation found; an empty vector means the declaration passed this check",
    r"Diagnostics for geometry or topology violations found; an empty vector means no finding was emitted",
    r"Contains the .* implementation and supporting types",
    r"Represents the `[^`]+` case",
)

def split_top_level(value: str) -> list[str]:
    parts: list[str] = []
    start = 0
    depth = {"(": 0, "[": 0, "{": 0, "<": 0}
    closing = {")": "(", "]": "[", "}": "{", ">": "<"}
    for index, character in enumerate(value):
        if character in depth:
            depth[character] += 1
        elif character in closing:
            key = closing[character]
            depth[key] = max(0, depth[key] - 1)
        elif character == "," and all(level == 0 for level in depth.values()):
            part = value[start:index].strip()
            if part:
                parts.append(part)
            start = index + 1
    tail = value[start:].strip()
    if tail:
        parts.append(tail)
    return parts

def preceding_docs(lines: list[str], index: int) -> str:
    cursor = index - 1
    while cursor >= 0 and (not lines[cursor].strip() or lines[cursor].lstrip().startswith("#[")):
        cursor -= 1
    comments: list[str] = []
    while cursor >= 0 and lines[cursor].lstrip().startswith("///"):
        comments.append(lines[cursor].strip()[3:].strip())
        cursor -= 1
    return "\n".join(reversed(comments))

def parse_signature(lines: list[str], index: int, name: str) -> tuple[list[str], str | None] | None:
    source = "\n".join(lines[index:])
    start = source.find(f"fn {name}")
    opening = source.find("(", start + len(name) + 3)
    if start < 0 or opening < 0:
        return None
    depth = 0
    closing = -1
    for position in range(opening, len(source)):
        if source[position] == "(":
            depth += 1
        elif source[position] == ")":
            depth -= 1
            if depth == 0:
                closing = position
                break
    if closing < 0:
        return None
    arguments = split_top_level(source[opening + 1 : closing])
    suffix = source[closing + 1 :]
    body_start = suffix.find("{")
    signature_suffix = suffix[:body_start] if body_start >= 0 else suffix
    arrow = signature_suffix.find("->")
    return_type = None
    if arrow >= 0:
        value = signature_suffix[arrow + 2 :]
        stops = [position for position in (value.find("where"), value.find(";")) if position >= 0]
        if stops:
            value = value[: min(stops)]
        return_type = re.sub(r"\s+", " ", value.strip())
        if return_type in ("", "()", "!"):
            return_type = None
    return arguments, return_type

def argument_name(argument: str) -> str | None:
    value = re.sub(r"^#\[[^\]]+\]\s*", "", argument.strip())
    if re.fullmatch(r"(?:&\s*(?:'[A-Za-z_]\w*)?\s*mut\s*)?self|mut self", value):
        return None
    colon = value.find(":")
    if colon < 0:
        return None
    name = re.sub(r"^(?:mut|ref)\s+", "", value[:colon].strip()).removeprefix("r#")
    return name if re.fullmatch(r"[A-Za-z_]\w*", name) else None

def validate(path: Path) -> list[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    failures: list[str] = []
    for index, line in enumerate(lines):
        if line.lstrip().startswith(("///", "//!")):
            comment = line.lstrip()[3:].strip()
            if any(re.search(placeholder, comment) for placeholder in PLACEHOLDER_DOC_PATTERNS):
                failures.append(
                    f"{path.relative_to(ROOT)}:{index + 1}: placeholder documentation is not allowed"
                )
    pattern = re.compile(r"^\s*(?:(?:pub(?:\([^)]*\))?)\s+)?(?:(?:async|const|unsafe|extern(?:\s+\"[^\"]+\")?)\s+)*fn\s+([A-Za-z_]\w*)\s*(?:<[^;{]*?>)?\s*\(")
    for index, line in enumerate(lines):
        match = pattern.match(line)
        if not match:
            continue
        name = match.group(1)
        location = f"{path.relative_to(ROOT)}:{index + 1} ({name})"
        docs = preceding_docs(lines, index)
        if not docs.strip():
            failures.append(f"{location}: missing doc comment")
            continue
        parsed = parse_signature(lines, index, name)
        if parsed is None:
            failures.append(f"{location}: could not parse signature")
            continue
        arguments, return_type = parsed
        names = [item for item in (argument_name(arg) for arg in arguments) if item]
        if names:
            if not re.search(r"^# Arguments\s*$", docs, flags=re.MULTILINE):
                failures.append(f"{location}: missing # Arguments")
            else:
                for item in names:
                    description = re.search(
                        rf"^\s*[-*]\s+`{re.escape(item)}`\s+—\s+(.+)$",
                        docs,
                        flags=re.MULTILINE,
                    )
                    if not description:
                        failures.append(f"{location}: argument `{item}` is undocumented")
                    elif len(description.group(1).split()) < 5:
                        failures.append(f"{location}: argument `{item}` needs a meaningful description")
        if return_type:
            if not re.search(r"^# Returns\s*$", docs, flags=re.MULTILINE):
                failures.append(f"{location}: missing # Returns")
            else:
                return_section = re.split(r"^# Returns\s*$", docs, flags=re.MULTILINE)[1]
                return_section = re.split(r"^# ", return_section, flags=re.MULTILINE)[0]
                if len(return_section.split()) < 5:
                    failures.append(f"{location}: # Returns must explain the returned value")
        if return_type and return_type.startswith("Result<") and not re.search(r"^# Errors\s*$", docs, flags=re.MULTILINE):
            failures.append(f"{location}: missing # Errors")
    return failures

def main() -> int:
    files = sorted(SOURCE_ROOT.rglob("*.rs"))
    failures = [failure for path in files for failure in validate(path)]
    if failures:
        print("Rust documentation contract failed:")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print(f"Rust documentation contract passed for {len(files)} Rust source files.")
    return 0

if __name__ == "__main__":
    sys.exit(main())
