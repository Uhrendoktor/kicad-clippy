#!/usr/bin/env python3
"""Reject nontrivial production floating-point literals outside named Rust constants."""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "src"
FLOAT_PATTERN = re.compile(
    r"(?<![A-Za-z0-9_])"
    r"(?:\d[\d_]*\.\d[\d_]*|\.\d[\d_]+)"
    r"(?:[eE][+-]?\d[\d_]*)?(?:f32|f64)?"
    r"(?![A-Za-z0-9_])"
)
CONST_DECLARATION = re.compile(
    r"^\s*(?:(?:pub(?:\([^)]*\))?)\s+)?const\s+[A-Za-z_]\w*\s*:"
)
IDENTITY_LITERALS = {"0.0", "1.0", "0.0_f64", "1.0_f64"}


def mask_non_code(source: str) -> str:
    """Blank comments and string literals without shifting Rust source line numbers."""
    output = list(source)
    index = 0
    state = "code"
    block_depth = 0
    raw_hashes = 0

    while index < len(source):
        character = source[index]
        following = source[index + 1] if index + 1 < len(source) else ""

        if state == "code":
            if character == "/" and following == "/":
                output[index] = output[index + 1] = " "
                index += 2
                state = "line"
                continue
            if character == "/" and following == "*":
                output[index] = output[index + 1] = " "
                index += 2
                state = "block"
                block_depth = 1
                continue

            prefix_start = index + 1 if character == "b" and following == "r" else index
            if source[prefix_start : prefix_start + 1] == "r":
                cursor = prefix_start + 1
                while cursor < len(source) and source[cursor] == "#":
                    cursor += 1
                if cursor < len(source) and source[cursor] == '"':
                    raw_hashes = cursor - prefix_start - 1
                    for position in range(index, cursor + 1):
                        if source[position] != "\n":
                            output[position] = " "
                    index = cursor + 1
                    state = "raw"
                    continue

            if character == '"' or (character == "b" and following == '"'):
                output[index] = " "
                if character == "b":
                    output[index + 1] = " "
                    index += 2
                else:
                    index += 1
                state = "string"
                continue

            index += 1
            continue

        if state == "line":
            if character == "\n":
                state = "code"
            else:
                output[index] = " "
            index += 1
            continue

        if state == "block":
            if character == "/" and following == "*":
                output[index] = output[index + 1] = " "
                block_depth += 1
                index += 2
                continue
            if character == "*" and following == "/":
                output[index] = output[index + 1] = " "
                block_depth -= 1
                index += 2
                if block_depth == 0:
                    state = "code"
                continue
            if character != "\n":
                output[index] = " "
            index += 1
            continue

        if state == "string":
            if character == "\\":
                output[index] = " "
                if index + 1 < len(source):
                    if source[index + 1] != "\n":
                        output[index + 1] = " "
                    index += 2
                else:
                    index += 1
                continue
            if character == '"':
                output[index] = " "
                index += 1
                state = "code"
                continue
            if character != "\n":
                output[index] = " "
            index += 1
            continue

        if state == "raw":
            terminator = '"' + ("#" * raw_hashes)
            if source.startswith(terminator, index):
                for position in range(index, index + len(terminator)):
                    output[position] = " "
                index += len(terminator)
                state = "code"
                continue
            if character != "\n":
                output[index] = " "
            index += 1

    return "".join(output)


def inspect(path: Path) -> list[str]:
    source = path.read_text(encoding="utf-8")
    test_start = source.find("#[cfg(test)]")
    if test_start >= 0:
        source = source[:test_start]

    masked_lines = mask_non_code(source).splitlines()
    source_lines = source.splitlines()
    failures: list[str] = []

    for line_number, code_line in enumerate(masked_lines, start=1):
        if CONST_DECLARATION.match(code_line):
            continue
        for match in FLOAT_PATTERN.finditer(code_line):
            literal = match.group(0)
            normalized = literal.removesuffix("f32").removesuffix("f64")
            if literal in IDENTITY_LITERALS or normalized in {"0.0", "1.0"}:
                continue
            excerpt = source_lines[line_number - 1].strip()
            failures.append(
                f"{path.relative_to(ROOT)}:{line_number}: inline float {literal}: {excerpt}"
            )
    return failures


def main() -> int:
    failures = [
        failure
        for path in sorted(SOURCE_ROOT.rglob("*.rs"))
        for failure in inspect(path)
    ]
    if failures:
        print("Named numeric constant check failed:")
        print("\n".join(f"- {failure}" for failure in failures))
        return 1
    print("Named numeric constant check passed: no nontrivial production floats remain inline.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
