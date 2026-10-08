# ADR 0034: Project license remains undecided

- Status: Deferred to owner
- Date: 2026-10-07

## Context

The M10 prompt offers permissive, copyleft and proprietary choices. Project scope describes personal use, while the public repository alone does not establish permission to reuse the code. The owner has not selected a license.

## Decision

Keep `CHOSEN LICENSE: undecided` in `docs/goals/M10.md`, do not add a `LICENSE` file, and provide a neutral comparison in `docs/LICENSE_OPTIONS.md`. Treat personal-only distribution as an assumption from `docs/PROJECT_SCOPE.md`, pending owner confirmation.

## Consequences

No reuse permission is asserted. Before sharing source or binaries, the owner must choose a license or maintain an explicit proprietary/all-rights-reserved position, review the dependency inventory, update package metadata/notices as needed, and run cargo-deny. The comparison is not legal advice. H.264 patent/licensing issues remain separate from the project's source license.
