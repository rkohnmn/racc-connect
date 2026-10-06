# ADR 0003: Use a host agent and a user-session app

## Status

Accepted

## Context

Hosting must continue independently of the UI, while the app owns viewer presentation and controls.

## Decision

Use two cooperating programs per machine: a background host-agent and a user-session app.

## Consequences

The app communicates with its local host-agent over local IPC and with remote host agents over the project protocol.