#!/usr/bin/env python3
"""Generate bundled third-party notices from Cargo's resolved release dependency graph."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
KNOWN_LICENSES = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "BSL-1.0", "CC0-1.0", "ISC", "Unicode-3.0", "Zlib", "Unlicense", "GPL-2.0-only", "GPL-3.0-only", "LGPL-2.1-or-later", "MPL-2.0", "OpenSSL", "CDLA-Permissive-2.0", "0BSD", "CC-BY-4.0", "CC-BY-SA-4.0"}
KNOWN_EXCEPTIONS = {"LLVM-exception", "Classpath-exception-2.0", "OpenSSL-exception"}
OPERATORS = {"AND", "OR", "WITH"}
APP_ROOTS = {"racc-app", "racc-host-agent"}


class NoticeError(Exception):
    pass


def license_ids(expression: str) -> set[str]:
    # Some older Cargo packages use a slash as an OR separator.
    tokens = re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*", expression.replace("/", " OR "))
    return {token for token in tokens if token not in OPERATORS}


def reachable(metadata: dict) -> list[dict]:
    packages = {package["id"]: package for package in metadata.get("packages", [])}
    nodes = {node["id"]: node for node in (metadata.get("resolve") or {}).get("nodes", [])}
    roots = [package_id for package_id, package in packages.items() if package.get("name") in APP_ROOTS]
    if not roots:
        raise NoticeError("Cargo metadata contains neither racc-app nor racc-host-agent")
    visited: set[str] = set()
    pending = list(roots)
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        node = nodes.get(package_id)
        if not node:
            continue
        for dependency in node.get("deps", []):
            kinds = dependency.get("dep_kinds") or [{"kind": None}]
            if any(kind.get("kind") != "dev" for kind in kinds):
                pending.append(dependency["pkg"])
    return sorted(
        (packages[package_id] for package_id in visited if package_id in packages and packages[package_id].get("source")),
        key=lambda package: (package.get("name", ""), package.get("version", "")),
    )


def license_files(package: dict) -> list[Path]:
    candidates: list[Path] = []
    explicit = package.get("license_file")
    if explicit:
        candidates.append(Path(explicit))
    manifest_dir = Path(package["manifest_path"]).parent
    if manifest_dir.exists():
        for item in manifest_dir.iterdir():
            if item.is_file() and re.match(r"^(license|copying)(?:[-_.].*)?$", item.name, re.IGNORECASE):
                candidates.append(item)
    unique: list[Path] = []
    seen: set[Path] = set()
    for candidate in candidates:
        candidate = candidate.resolve()
        if candidate.is_file() and candidate not in seen:
            seen.add(candidate)
            unique.append(candidate)
    return unique


def artwork_attribution(path: Path = ROOT / "docs" / "ASSETS.md") -> str:
    try:
        source = path.read_text(encoding="utf-8").strip()
    except OSError as error:
        raise NoticeError(f"artwork attribution could not be read from {path}: {error}") from error
    if not source or "## Raccoon mark" not in source:
        raise NoticeError(f"artwork attribution is missing the Raccoon mark section: {path}")
    return source


def build_notice(metadata: dict, assets_file: Path = ROOT / "docs" / "ASSETS.md") -> str:
    packages = reachable(metadata)
    unknown: list[str] = []
    inventory: list[tuple[dict, str, list[Path]]] = []
    text_by_digest: dict[str, tuple[str, list[str]]] = {}
    for package in packages:
        expression = package.get("license")
        if not expression:
            unknown.append(f"{package['name']} {package['version']}: no SPDX license expression")
            continue
        ids = license_ids(expression)
        exceptions = {token for token in ids if token in KNOWN_EXCEPTIONS}
        unsupported = ids - KNOWN_LICENSES - exceptions
        if not ids or unsupported:
            unknown.append(f"{package['name']} {package['version']}: unrecognized license expression {expression!r}")
            continue
        files = license_files(package)
        inventory.append((package, expression, files))
        for path in files:
            text = path.read_text(encoding="utf-8", errors="replace").strip()
            if not text:
                continue
            digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            if digest not in text_by_digest:
                text_by_digest[digest] = (text, [])
            text_by_digest[digest][1].append(f"{package['name']} {package['version']} (`{path.name}`)")
    if unknown:
        raise NoticeError("Unrecognized dependency licenses:\n- " + "\n- ".join(unknown))
    lines = [
        "# Third-party license notices",
        "",
        "Generated from the resolved non-development dependency graph of `racc-app` and `racc-host-agent` with `cargo metadata --format-version 1 --locked --offline`.",
        "Generation requires recognized SPDX license identifiers. `--check` also compares this file with the current resolved dependency graph and artwork provenance; the repository's unchanged `cargo deny` policy independently enforces which licenses may ship.",
        "This inventory is attribution information, not legal advice.",
        "",
        "## Resolved package inventory",
        "",
        "| Package | Version | License | Bundled license text found |",
        "|---|---:|---|---|",
    ]
    for package, expression, files in inventory:
        lines.append(f"| `{package['name']}` | `{package['version']}` | `{expression}` | {'yes' if files else 'no'} |")
    artwork = artwork_attribution(assets_file)
    nested_artwork = "\n".join(
        ("#" + line if line.startswith("# ") else "#" + line if line.startswith("## ") else line)
        for line in artwork.splitlines()
    )
    lines.extend([
        "",
        "## Icon artwork and attribution",
        "",
        "The following artwork source, modifications, and licensing status are synchronized from `docs/ASSETS.md`.",
        "",
        nested_artwork,
        "",
        "## License texts found in the resolved source tree",
        "",
    ])
    if not text_by_digest:
        lines.append("No license text files were present in the resolved package source directories; package license metadata is listed above.")
    else:
        for digest, (text, sources) in sorted(text_by_digest.items(), key=lambda item: item[1][1][0].casefold()):
            lines.extend([f"### SHA-256 `{digest}`", "", "Found in: " + ", ".join(sorted(set(sources))), "", "```text", text, "```", ""])
    return "\n".join(lines).rstrip() + "\n"


def load_metadata(path: Path | None) -> dict:
    if path:
        return json.loads(path.read_text(encoding="utf-8"))
    try:
        target_metadata = []
        for target in ("x86_64-pc-windows-msvc", "x86_64-apple-darwin"):
            result = subprocess.run(
                ["cargo", "metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", target],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
            target_metadata.append(json.loads(result.stdout))
        packages_by_id = {}
        nodes_by_id = {}
        for metadata in target_metadata:
            packages_by_id.update({package["id"]: package for package in metadata.get("packages", [])})
            for node in (metadata.get("resolve") or {}).get("nodes", []):
                existing = nodes_by_id.setdefault(node["id"], {"id": node["id"], "deps": []})
                seen = {json.dumps(dependency, sort_keys=True) for dependency in existing["deps"]}
                for dependency in node.get("deps", []):
                    encoded = json.dumps(dependency, sort_keys=True)
                    if encoded not in seen:
                        existing["deps"].append(dependency)
                        seen.add(encoded)
        return {"packages": list(packages_by_id.values()), "resolve": {"nodes": list(nodes_by_id.values())}}
    except (OSError, subprocess.CalledProcessError) as error:
        raise NoticeError(f"cargo metadata failed: {error}") from error
    return json.loads(result.stdout)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / "THIRD_PARTY_LICENSES.md")
    parser.add_argument("--metadata-file", type=Path, help="use a fixture Cargo metadata document")
    parser.add_argument("--assets-file", type=Path, default=ROOT / "docs" / "ASSETS.md", help="artwork provenance source")
    parser.add_argument("--check", action="store_true", help="fail unless the notice file matches current metadata and artwork attribution")
    args = parser.parse_args()
    try:
        notice = build_notice(load_metadata(args.metadata_file), args.assets_file)
    except (NoticeError, json.JSONDecodeError, OSError) as error:
        print(f"notice generation failed: {error}", file=sys.stderr)
        return 1
    if args.check:
        try:
            current = args.output.read_text(encoding="utf-8")
        except OSError as error:
            print(f"notice check failed: cannot read {args.output}: {error}", file=sys.stderr)
            return 1
        if current != notice:
            print(
                f"notice check failed: {args.output} is stale; regenerate it with scripts/gen-notices.py",
                file=sys.stderr,
            )
            return 1
        print("Notice file matches current release dependencies and artwork attribution.")
        return 0
    args.output.write_text(notice, encoding="utf-8", newline="\n")
    print(f"Wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

