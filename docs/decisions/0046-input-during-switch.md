# ADR 0046: Suppress pointer input during display switching

## Status

Accepted for M3b.

## Context

A display switch keeps the previous decoded image visible until a keyframe for the new epoch is ready. During that interval, pointer coordinates over the held image do not describe the host display that will receive input.

## Decision

Suppress pointer motion and pointer-button events while a switch is pending. Continue forwarding key and wheel events because they do not depend on the selected display's absolute geometry. Resume pointer input only after the matching epoch's keyframe is atomically promoted.

## Consequences

This avoids injecting pointer events against stale geometry. The `ViewerSession::on_user_input` reducer now enforces this policy and emits only metadata actions. The runtime still must route those actions without blocking input or frame presentation; the controller policy is covered by M3b tests.