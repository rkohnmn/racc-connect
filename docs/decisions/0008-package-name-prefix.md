# ADR 0008: Prefix Cargo package names with racc-

## Status

Accepted; supersedes ADR 0002.

## Context

The project is named Racc Connect. Crate directory names remain those in AGENTS.md, while Cargo package names need a prefix because a package named `core` conflicts with Rust's built-in crate.

## Decision

Prefix every Cargo package name with `racc-`. Keep crate directory names unchanged.

## Consequences

Cargo package names include `racc-proto`, `racc-core`, `racc-app`, and `racc-host-agent`. Commands and references that use Cargo package names use the new prefix.
