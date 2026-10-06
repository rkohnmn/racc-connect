# ADR 0005: TCP control and UDP video

## Status

Accepted

## Context

The protocol uses separate control and video channels.

## Decision

Carry control messages over length-prefixed TCP with TCP_NODELAY and carry video datagrams over UDP. Cap each video datagram at 1200 bytes including its header.

## Consequences

Control is reliable and video loss recovery uses incomplete-frame dropping and keyframe requests in v0.