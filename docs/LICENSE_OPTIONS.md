# Project license options

**This comparison is informational and is not legal advice.** The owner has not selected a project license. No `LICENSE` file is added and the repository must not be presented as granting reuse rights until a license decision is made.

The project is currently described in `docs/PROJECT_SCOPE.md` as a private-network tool for the author's three personal machines. M10 assumes personal-only distribution for packaging decisions, pending owner confirmation. Sharing builds would make publisher identity, support expectations, and license clarity more important.

| Option | What it generally permits/requires | Fit and tradeoffs to review |
|---|---|---|
| MIT | Broad reuse, modification and redistribution with copyright/license notice; limited express patent wording. | Simple for a small utility and broadly compatible with the permissive dependencies. It does not itself solve H.264 patent or codec licensing questions. |
| Apache-2.0 | Permissive reuse with explicit patent grant and conditions such as preserving notices and marking changes. | More detailed patent language than MIT; compatible with the current permissive dependency set. The grant is not a substitute for investigating third-party codec patent pools or patent rights. |
| MIT OR Apache-2.0 | Lets downstream users choose either license for the project code. | Common Rust-project choice and compatible with current dependencies. It requires retaining both license texts and still does not settle codec-related patent questions. |
| GPL-3.0-only | Copyleft obligations apply to distribution of covered combined works; includes patent-related terms. | The repository's own `deny.toml` forbids GPL dependencies, but that dependency policy is distinct from choosing a project license. Review compatibility with iced and all bundled dependencies before any choice. H.264 patent questions remain separate. |
| AGPL-3.0-only | GPL-like copyleft with additional network-interaction source offer conditions for modified versions. | Likely broader obligations than desired for a personal tool shared over a network. `deny.toml` forbids AGPL dependencies; selecting an AGPL project license would still require a deliberate compatibility review. |
| Proprietary / all rights reserved | No general permission to reuse or redistribute absent separate permission. | Consistent with private personal use, but a public repository without a license already does not grant broad reuse rights. A proprietary notice should be explicit and reviewed before distribution. |

## Dependency and toolkit notes

The current `deny.toml` allows the permissive/public-domain SPDX identifiers observed in the dependency graph and rejects other license identifiers. ADR 0001 records iced as MIT-licensed and notes that Slint's chosen royalty-free route would carry attribution conditions; the project selected iced. Generated notices list the resolved shipped dependency licenses and license texts found locally.

The Windows software codec is OpenH264, whose source code has its own BSD license; distribution and binary use may have separate patent/patent-pool or build-distribution considerations. The project excludes x264 (GPL). Hardware H.264 encode/decode uses OS/vendor components. None of these observations establishes that a project license grants patent rights for H.264. The owner should obtain appropriate legal advice before sharing binaries or code if patent exposure matters.

## Decision still needed

The M10 prompt records `CHOSEN LICENSE: undecided` and `DISTRIBUTION: personal-only (assumption from project scope; owner confirmation pending)`. To choose a license, edit those values, add the exact license file(s), update Cargo package metadata and notices, and run `cargo deny check licenses`. Do not infer the choice from this comparison.
