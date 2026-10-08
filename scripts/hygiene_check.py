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


def git_bytes(*args: str) -> bytes:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True).stdout


def check_content(content: bytes, origin: str, findings: list[str]) -> None:
    for name, pattern in PATTERNS.items():
        if pattern.search(content):
            findings.append(f"{origin}: {name}")


def scan_worktree(findings: list[str]) -> None:
    for raw in git_bytes("ls-files", "-co", "--exclude-standard").splitlines():
        relative = raw.decode("utf-8", errors="replace")
        if any(part in EXCLUDED_PARTS for part in Path(relative).parts):
            continue
        path = ROOT / relative
        if path.is_file():
            try:
                check_content(path.read_bytes(), f"working tree {relative}", findings)
            except OSError:
                continue


def scan_history(findings: list[str]) -> None:
    commits = git_bytes("rev-list", "--all").decode("ascii", errors="replace").splitlines()
    blobs: dict[str, str] = {}
    for commit in commits:
        for record in git_bytes("ls-tree", "-r", "-z", commit).split(b"\x00"):
            if not record:
                continue
            metadata, raw_path = record.split(b"\t", 1)
            mode, kind, object_id = metadata.split(b" ", 2)
            if kind == b"blob" and mode not in (b"120000",):
                blobs.setdefault(object_id.decode("ascii"), raw_path.decode("utf-8", errors="replace"))
    object_ids = list(blobs)
    for offset in range(0, len(object_ids), 256):
        chunk = object_ids[offset:offset + 256]
        output = subprocess.run(
            ["git", "cat-file", "--batch"], cwd=ROOT, input=("\n".join(chunk) + "\n").encode("ascii"),
            check=True, capture_output=True,
        ).stdout
        cursor = 0
        for object_id in chunk:
            newline = output.find(b"\n", cursor)
            if newline < 0:
                raise RuntimeError("malformed git cat-file batch header")
            header = output[cursor:newline].split()
            if len(header) != 3 or header[1] != b"blob":
                raise RuntimeError("unexpected non-blob in git cat-file batch")
            size = int(header[2])
            start, end = newline + 1, newline + 1 + size
            check_content(output[start:end], f"history blob {object_id[:12]} {blobs[object_id]}", findings)
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
    print("Hygiene scan passed: no configured secret or personal-path patterns were found in the worktree or reachable Git history.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
