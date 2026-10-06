# ADR 0006: Relaxed zero-copy policy

## Status

Accepted

## Context

True zero-copy is not required for the initial supported stream resolutions.

## Decision

Avoid CPU copies where easy. One GPU copy or a plain NV12 upload to the GPU is acceptable; true zero-copy is a later optimization.

## Consequences

The first renderer may use a straightforward GPU upload path.