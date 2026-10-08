#!/usr/bin/env python3
"""Write stable, basename-only SHA-256 sidecars for release artifacts."""
from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import tempfile
from pathlib import Path
from typing import Iterable


def sha256_file(path: Path) -> str:
    """Return the SHA-256 digest of a file without loading it all into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as artifact:
        for chunk in iter(lambda: artifact.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_sidecars(paths: Iterable[Path]) -> list[tuple[Path, str]]:
    """Write one GNU-compatible ``.sha256`` sidecar beside each artifact."""
    results: list[tuple[Path, str]] = []
    for raw_path in paths:
        path = raw_path.resolve(strict=True)
        if not path.is_file():
            raise ValueError(f"artifact is not a regular file: {path}")
        if not path.name.isascii() or "\n" in path.name or "\r" in path.name:
            raise ValueError(f"artifact name must be ASCII without line breaks: {path.name!r}")

        digest = sha256_file(path)
        sidecar = path.with_name(path.name + ".sha256")
        temporary_path: Path | None = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w",
                encoding="ascii",
                newline="\n",
                dir=sidecar.parent,
                prefix=sidecar.name + ".",
                suffix=".tmp",
                delete=False,
            ) as temporary:
                temporary.write(f"{digest}  {path.name}\n")
                temporary.flush()
                os.fsync(temporary.fileno())
                temporary_path = Path(temporary.name)
            shutil.copymode(path, temporary_path)
            os.replace(temporary_path, sidecar)
        finally:
            if temporary_path is not None:
                temporary_path.unlink(missing_ok=True)

        results.append((sidecar, digest))
        print(f"SHA-256: {digest}  {path.name}")
    return results


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", nargs="+", type=Path)
    args = parser.parse_args()
    write_sidecars(args.artifacts)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
