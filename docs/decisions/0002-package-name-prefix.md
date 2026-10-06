# ADR 0002: Prefix Cargo package names with rd-

## Status

Superseded by ADR 0008

## Context

The workspace contains a directory named core, and a package named core conflicts with Rust's built-in crate name.

## Decision

Keep the required crate directory names and prefix every Cargo package name with rd-.

## Consequences

Cargo package names include rd-proto, rd-core, rd-app, and rd-host-agent. References using Cargo package names must use the prefix.