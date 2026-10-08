# ADR 0043: Report host quality target changes to the active viewer

## Status

Accepted for protocol v4.

## Decision

Add control message type 19, `QualityAdjustment`, to report the host policy's selected bitrate or tier target to the active viewer. It carries the stream epoch, reason, source and target heights, and source and target bitrates. A bitrate trim remains in the same epoch; a tier adjustment follows the successful `StreamReset` and uses its new epoch. The viewer records only an adjustment that matches its active epoch.

## Compatibility

The message changes the control-message set and therefore bumps `PROTOCOL_VERSION` from 3 to 4. Version 4 peers are incompatible with versions 0 through 3 and must be upgraded together. Bitrate values are bounded to 70–100% of the documented default for one of the supported 480p30, 720p30, or 1080p30 tiers.

## Limits

This reports the controller target, not a measured encoder output bitrate. It does not prove the host encoder applied the requested value or that a network path improved.
