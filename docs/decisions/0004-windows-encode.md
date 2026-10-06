# ADR 0004: Windows Media Foundation encode with OpenH264 fallback

## Status

Accepted

## Context

Windows encoding must support hardware H.264 paths without linking GPL x264.

## Decision

Use the Media Foundation hardware H.264 encoder by default and OpenH264 as the software fallback. Do not link x264.

## Consequences

Direct vendor encoder SDKs remain a later optimization behind the encoder interface.