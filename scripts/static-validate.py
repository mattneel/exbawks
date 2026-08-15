#!/usr/bin/env python3
"""Run repository checks that do not require a Rust toolchain."""

from __future__ import annotations

import argparse
import ast
import hashlib
import importlib.util
import json
import re
import sys
import tomllib
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable

TEXT_SUFFIXES = {
    ".json",
    ".md",
    ".ps1",
    ".py",
    ".rs",
    ".toml",
    ".txt",
    ".yml",
    ".yaml",
}
EXPECTED_FILES = {
    "AGENTS.md",
    "Cargo.toml",
    "README.md",
    "VALIDATION.md",
    "docs/README.md",
    "docs/agent-handoff.md",
    "docs/architecture.md",
    "docs/codegen-contract.md",
    "docs/memory.md",
    "docs/repository-manifest.md",
    "docs/security-model.md",
    "docs/task-board.md",
    "fixtures/synthetic/minimal-retail.xbe",
}
MARKDOWN_LINK = re.compile(r"!?(?:\[[^\]]*\])\(([^)]+)\)")
MODULE_DECLARATION = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")


@dataclass(frozen=True)
class CheckResult:
    name: str
    status: str
    detail: str


class OptionalCheckUnavailable(RuntimeError):
    """Indicate that an optional parser is not installed."""


class Validation:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.results: list[CheckResult] = []

    def run(self, name: str, check: Callable[[], str]) -> None:
        try:
            detail = check()
        except OptionalCheckUnavailable as error:
            self.results.append(CheckResult(name, "warn", str(error)))
        except Exception as error:  # noqa: BLE001
            self.results.append(CheckResult(name, "fail", str(error)))
        else:
            self.results.append(CheckResult(name, "pass", detail))

    def warn(self, name: str, detail: str) -> None:
        self.results.append(CheckResult(name, "warn", detail))

    @property
    def failed(self) -> bool:
        return any(result.status == "fail" for result in self.results)


def relative(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def text_files(root: Path) -> list[Path]:
    return sorted(
        path
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in TEXT_SUFFIXES
    )


def check_expected_files(root: Path) -> str:
    missing = sorted(path for path in EXPECTED_FILES if not (root / path).is_file())
    if missing:
        raise ValueError(f"missing required files: {', '.join(missing)}")
    return f"found {len(EXPECTED_FILES)} required files"


def check_text_hygiene(root: Path) -> str:
    errors: list[str] = []
    count = 0
    for path in text_files(root):
        count += 1
        data = path.read_bytes()
        if b"\x00" in data:
            errors.append(f"{relative(root, path)} contains a NUL byte")
            continue
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError as error:
            errors.append(f"{relative(root, path)} is not UTF-8: {error}")
            continue
        if text and not text.endswith("\n"):
            errors.append(f"{relative(root, path)} lacks a final newline")
        for number, line in enumerate(text.splitlines(), start=1):
            if line.rstrip(" \t") != line:
                errors.append(f"{relative(root, path)}:{number} has trailing whitespace")
            if "\t" in line:
                errors.append(f"{relative(root, path)}:{number} contains a tab")
    if errors:
        raise ValueError("; ".join(errors[:20]))
    return f"checked {count} UTF-8 text files"


def check_toml(root: Path) -> str:
    files = sorted(root.rglob("*.toml"))
    for path in files:
        with path.open("rb") as stream:
            tomllib.load(stream)
    return f"parsed {len(files)} TOML files"


def check_yaml(root: Path) -> str:
    try:
        import yaml  # type: ignore[import-untyped]
    except ImportError as error:
        raise OptionalCheckUnavailable("skipped YAML parsing because PyYAML is unavailable") from error

    files = sorted([*root.rglob("*.yml"), *root.rglob("*.yaml")])
    for path in files:
        with path.open("r", encoding="utf-8") as stream:
            yaml.safe_load(stream)
    return f"parsed {len(files)} YAML files"


def check_python(root: Path) -> str:
    files = sorted(root.rglob("*.py"))
    for path in files:
        ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    return f"parsed {len(files)} Python files"


def check_workspace(root: Path) -> str:
    with (root / "Cargo.toml").open("rb") as stream:
        workspace = tomllib.load(stream)
    members = workspace.get("workspace", {}).get("members", [])
    if not isinstance(members, list) or not members:
        raise ValueError("workspace.members is empty")

    package_names: set[str] = set()
    for member in members:
        manifest = root / member / "Cargo.toml"
        if not manifest.is_file():
            raise ValueError(f"workspace member lacks Cargo.toml: {member}")
        with manifest.open("rb") as stream:
            data = tomllib.load(stream)
        package = data.get("package", {})
        name = package.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError(f"workspace member lacks package.name: {member}")
        if name in package_names:
            raise ValueError(f"duplicate package name: {name}")
        package_names.add(name)

        targets = data.get("bin", [])
        if targets:
            for target in targets:
                source = root / member / target.get("path", "src/main.rs")
                if not source.is_file():
                    raise ValueError(f"missing binary source: {relative(root, source)}")
        elif not (root / member / "src/lib.rs").is_file() and not (
            root / member / "src/main.rs"
        ).is_file():
            raise ValueError(f"workspace member has no crate root: {member}")

    return f"validated {len(members)} workspace members"


def check_rust_modules(root: Path) -> str:
    declarations = 0
    for path in sorted(root.rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for name in MODULE_DECLARATION.findall(text):
            declarations += 1
            direct = path.parent / f"{name}.rs"
            nested = path.parent / name / "mod.rs"
            sibling_directory = path.parent / path.stem / f"{name}.rs"
            sibling_nested = path.parent / path.stem / name / "mod.rs"
            candidates = (direct, nested, sibling_directory, sibling_nested)
            if not any(candidate.is_file() for candidate in candidates):
                raise ValueError(
                    f"{relative(root, path)} declares missing module {name!r}"
                )
    return f"resolved {declarations} Rust module declarations"


def skip_rust_literal(text: str, index: int) -> int | None:
    if text.startswith('b"', index):
        index += 1
    if index < len(text) and text[index] == '"':
        index += 1
        while index < len(text):
            if text[index] == "\\":
                index += 2
            elif text[index] == '"':
                return index + 1
            else:
                index += 1
        return len(text)

    raw_match = re.match(r"(?:br|rb|r)(#+)?\"", text[index:])
    if raw_match:
        hashes = raw_match.group(1) or ""
        end_marker = '"' + hashes
        start = index + raw_match.end()
        end = text.find(end_marker, start)
        return len(text) if end < 0 else end + len(end_marker)

    if text.startswith("b'", index):
        index += 1
    if index < len(text) and text[index] == "'":
        cursor = index + 1
        if cursor < len(text) and text[cursor] == "\\":
            cursor += 2
        else:
            cursor += 1
        if cursor < len(text) and text[cursor] == "'":
            return cursor + 1
    return None


def check_rust_delimiters(root: Path) -> str:
    pairs = {"(": ")", "[": "]", "{": "}"}
    closers = set(pairs.values())
    files = sorted(root.rglob("*.rs"))
    for path in files:
        text = path.read_text(encoding="utf-8")
        stack: list[tuple[str, int]] = []
        index = 0
        block_comment_depth = 0
        while index < len(text):
            if block_comment_depth:
                if text.startswith("/*", index):
                    block_comment_depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    block_comment_depth -= 1
                    index += 2
                else:
                    index += 1
                continue
            if text.startswith("//", index):
                newline = text.find("\n", index + 2)
                index = len(text) if newline < 0 else newline + 1
                continue
            if text.startswith("/*", index):
                block_comment_depth = 1
                index += 2
                continue
            literal_end = skip_rust_literal(text, index)
            if literal_end is not None:
                index = literal_end
                continue
            character = text[index]
            if character in pairs:
                stack.append((character, index))
            elif character in closers:
                if not stack or pairs[stack[-1][0]] != character:
                    raise ValueError(
                        f"{relative(root, path)} has an unmatched {character!r}"
                    )
                stack.pop()
            index += 1
        if block_comment_depth:
            raise ValueError(f"{relative(root, path)} has an open block comment")
        if stack:
            opener, offset = stack[-1]
            line = text.count("\n", 0, offset) + 1
            raise ValueError(
                f"{relative(root, path)}:{line} has an unmatched {opener!r}"
            )
    return f"balanced delimiters in {len(files)} Rust files"


def check_markdown_links(root: Path) -> str:
    checked = 0
    for path in sorted(root.rglob("*.md")):
        text = path.read_text(encoding="utf-8")
        for raw_target in MARKDOWN_LINK.findall(text):
            target = raw_target.strip().split()[0].strip("<>")
            if target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            target = target.split("#", 1)[0]
            if not target:
                continue
            checked += 1
            resolved = (path.parent / target).resolve()
            try:
                resolved.relative_to(root.resolve())
            except ValueError as error:
                raise ValueError(
                    f"{relative(root, path)} links outside the repository: {target}"
                ) from error
            if not resolved.exists():
                raise ValueError(
                    f"{relative(root, path)} has a missing link target: {target}"
                )
    return f"resolved {checked} local Markdown links"


def load_fixture_generator(root: Path):
    path = root / "scripts/make-synthetic-xbe.py"
    spec = importlib.util.spec_from_file_location("exbawks_fixture", path)
    if spec is None or spec.loader is None:
        raise ValueError("cannot load the synthetic fixture generator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def check_fixture(root: Path) -> str:
    module = load_fixture_generator(root)
    expected = module.make_image()
    path = root / "fixtures/synthetic/minimal-retail.xbe"
    actual = path.read_bytes()
    if actual != expected:
        raise ValueError("synthetic XBE does not match its generator")
    digest = hashlib.sha256(actual).hexdigest()
    return f"fixture matches generator; sha256={digest}"


def check_unsafe_comments(root: Path) -> str:
    files = sorted(root.rglob("*.rs"))
    unsafe_blocks = 0
    for path in files:
        lines = path.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if re.search(r"\bunsafe\s*\{", line) is None:
                continue
            unsafe_blocks += 1
            context = "\n".join(lines[max(0, index - 3) : index])
            if "SAFETY:" not in context:
                raise ValueError(
                    f"{relative(root, path)}:{index + 1} lacks a nearby SAFETY comment"
                )
    return f"checked {unsafe_blocks} unsafe blocks"


def check_private_data_rules(root: Path) -> str:
    forbidden_suffixes = {".iso", ".xbe.orig", ".bin", ".rom"}
    violations = []
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        rel = relative(root, path)
        if rel == "fixtures/synthetic/minimal-retail.xbe":
            continue
        if any(rel.lower().endswith(suffix) for suffix in forbidden_suffixes):
            violations.append(rel)
    if violations:
        raise ValueError(f"possible private binary data: {', '.join(violations)}")
    return "found no forbidden fixture extensions"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()

    validation = Validation(root)
    validation.run("required-files", lambda: check_expected_files(root))
    validation.run("text-hygiene", lambda: check_text_hygiene(root))
    validation.run("toml", lambda: check_toml(root))
    validation.run("yaml", lambda: check_yaml(root))
    validation.run("python", lambda: check_python(root))
    validation.run("workspace", lambda: check_workspace(root))
    validation.run("rust-modules", lambda: check_rust_modules(root))
    validation.run("rust-delimiters", lambda: check_rust_delimiters(root))
    validation.run("markdown-links", lambda: check_markdown_links(root))
    validation.run("synthetic-fixture", lambda: check_fixture(root))
    validation.run("unsafe-comments", lambda: check_unsafe_comments(root))
    validation.run("private-data-rules", lambda: check_private_data_rules(root))

    if args.json:
        print(json.dumps([asdict(result) for result in validation.results], indent=2))
    else:
        for result in validation.results:
            print(f"{result.status.upper():4}  {result.name:22}  {result.detail}")

    return 1 if validation.failed else 0


if __name__ == "__main__":
    sys.exit(main())
