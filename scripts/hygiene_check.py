#!/usr/bin/env python3
"""Scan the worktree and reachable Git history without printing matched content."""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PATTERNS = {
    "private-key-material": re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
    "tailscale-auth-key": re.compile(rb"tskey-(?:auth|api)-[A-Za-z0-9_-]{12,}"),
    "aws-access-key": re.compile(rb"(?:AKIA|ASIA)[A-Z0-9]{16}"),
    "github-token": re.compile(rb"gh[pousr]_[A-Za-z0-9]{30,}"),
    "personal-windows-profile-path": re.compile(rb"(?i)[A-Z]:\\Users\\[^\s\"']+"),
}
EXCLUDED_PARTS = {".git", "target", "node_modules", "__pycache__"}

# Two reviewed blobs in the public history of TRANSPORT_TRACE_M2_5.txt contain
# a personal Windows profile path. Their replacement is already in the current
# tree. Keep this baseline exact: worktree findings are never exempt, and a
# changed object, path, or rule is reported for review.
HISTORY_BASELINE = frozenset({
    ("c0de3017b472cbcc654d6803669def349d8e9e79", "docs/TRANSPORT_TRACE_M2_5.txt", "personal-windows-profile-path"),
    ("70b87fdf00740cd18eecb50bf71dfe2a0389f332", "docs/TRANSPORT_TRACE_M2_5.txt", "personal-windows-profile-path"),
})


def is_baselined_history_finding(object_id: str, path: str, rule: str) -> bool:
    """Return true only for one of the two explicitly reviewed history blobs."""
    return (object_id, path, rule) in HISTORY_BASELINE


def git_bytes(*args: str) -> bytes:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True).stdout


def content_findings(content: bytes, origin: str) -> list[str]:
    findings: list[str] = []
    for name, pattern in PATTERNS.items():
        if pattern.search(content):
            findings.append(f"{origin}: {name}")
    return findings


def unreviewed_history_findings(object_id: str, path: str, content: bytes) -> list[str]:
    origin = f"history blob {object_id[:12]} {path}"
    findings = content_findings(content, origin)
    return [
        finding for finding in findings
        if not is_baselined_history_finding(object_id, path, finding.rsplit(": ", 1)[-1])
    ]


def scan_worktree(findings: list[str]) -> None:
    for raw in git_bytes("ls-files", "-co", "--exclude-standard").splitlines():
        relative = raw.decode("utf-8", errors="replace")
        if any(part in EXCLUDED_PARTS for part in Path(relative).parts):
            continue
        path = ROOT / relative
        if path.is_file():
            try:
                findings.extend(content_findings(path.read_bytes(), f"working tree {relative}"))
            except OSError:
                continue


def scan_history(findings: list[str]) -> None:
    commits = git_bytes("rev-list", "--all").decode("ascii", errors="replace").splitlines()
    blobs: set[tuple[str, str]] = set()
    for commit in commits:
        for record in git_bytes("ls-tree", "-r", "-z", commit).split(b"\x00"):
            if not record:
                continue
            metadata, raw_path = record.split(b"\t", 1)
            mode, kind, object_id = metadata.split(b" ", 2)
            if kind == b"blob" and mode not in (b"120000",):
                blobs.add((object_id.decode("ascii"), raw_path.decode("utf-8", errors="replace")))
    blob_paths = sorted(blobs)
    for offset in range(0, len(blob_paths), 256):
        chunk = blob_paths[offset:offset + 256]
        object_ids = [object_id for object_id, _ in chunk]
        output = subprocess.run(
            ["git", "cat-file", "--batch"], cwd=ROOT, input=("\n".join(object_ids) + "\n").encode("ascii"),
            check=True, capture_output=True,
        ).stdout
        cursor = 0
        for object_id, path in chunk:
            newline = output.find(b"\n", cursor)
            if newline < 0:
                raise RuntimeError("malformed git cat-file batch header")
            header = output[cursor:newline].split()
            if len(header) != 3 or header[1] != b"blob":
                raise RuntimeError("unexpected non-blob in git cat-file batch")
            size = int(header[2])
            start, end = newline + 1, newline + 1 + size
            findings.extend(unreviewed_history_findings(object_id, path, output[start:end]))
            cursor = end + 1


def main() -> int:
    findings: list[str] = []
    try:
        scan_worktree(findings)
        scan_history(findings)
    except (OSError, subprocess.CalledProcessError, RuntimeError) as error:
        print(f"hygiene scan failed: {error}", file=sys.stderr)
        return 2
    if findings:
        print("Potential private data found (only rule names and file/object paths are shown):", file=sys.stderr)
        for finding in findings:
            print(f"- {finding}", file=sys.stderr)
        return 1
    print(
        "Hygiene scan passed: no configured secret or personal-path patterns were found in the worktree or unreviewed reachable Git history. "
        f"Explicitly baselined historical blobs: {len(HISTORY_BASELINE)}."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
