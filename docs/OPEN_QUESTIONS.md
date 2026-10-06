# Open Questions

Record unresolved product, architecture, environment, or instruction conflicts here. Resolve with the project author before making assumptions that affect implementation.

1. What is the final project working title?
2. Which project license should apply? No license has been chosen.
3. Which Tailscale LocalAPI access method must the identity crate use on each supported platform and installation flavor?
4. Which UI toolkit will the M4a spike select, based on measured evidence?
5. The attached M0 objective says not to mention the product inspiration in any file, while AGENTS.md section 13 permits that reference in AGENTS.md and the existing project-scope text already contains it. Preserve the authoritative source text and avoid adding such references to new product-facing files.
6. AGENTS.md section 10.3 asks extra verification scripts to be listed there, while the M0 objective otherwise restricts AGENTS.md edits. The required script listing was added alongside the explicitly requested session-start and package-prefix notes.
7. docs/DEV_SETUP.md still lists cargo deny check as the manual command; the M0 verification scripts use cargo deny check --warn advisories so security advisory findings are warnings. Reconcile the manual command if warning-only behavior is desired there too.