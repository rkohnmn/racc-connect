//! Annex B to AVCC conversion and parameter-set tracking for VideoToolbox decoding.
//!
//! The helper is portable and testable; the native VTDecompressionSession adapter remains
//! unimplemented until it can be exercised on the Monterey Mac.

use crate::{DecodeError, EncodedAccessUnit};

const MAX_NAL_UNITS: usize = 4096;

/// One bounded AVCC sample and the latest parameter sets needed to describe it to VideoToolbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoToolboxSample {
    /// Four-byte big-endian length-prefixed H.264 NAL units.
    pub avcc: Vec<u8>,
    /// Most recent sequence parameter set, if present in this sample or cached earlier.
    pub sps: Option<Vec<u8>>,
    /// Most recent picture parameter set, if present in this sample or cached earlier.
    pub pps: Option<Vec<u8>>,
    /// Whether the sample introduces or changes parameter sets.
    pub configuration_changed: bool,
}

/// Converts a validated Annex B access unit to AVCC and tracks parameter sets between frames.
#[derive(Clone, Debug, Default)]
pub struct VideoToolboxBitstream {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

impl VideoToolboxBitstream {
    /// Creates an empty parameter-set cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Normalizes one access unit. IDR frames require SPS and PPS to be known.
    pub fn sample(
        &mut self,
        access_unit: &EncodedAccessUnit,
    ) -> Result<VideoToolboxSample, DecodeError> {
        let nals = parse_annex_b(&access_unit.bytes)?;
        let mut changed = false;
        for nal in &nals {
            match nal[0] & 0x1f {
                7 if self.sps.as_deref() != Some(*nal) => {
                    self.sps = Some(nal.to_vec());
                    changed = true;
                }
                8 if self.pps.as_deref() != Some(*nal) => {
                    self.pps = Some(nal.to_vec());
                    changed = true;
                }
                _ => {}
            }
        }
        if access_unit.is_keyframe() && (self.sps.is_none() || self.pps.is_none()) {
            return Err(DecodeError::MissingCodecConfiguration);
        }
        let mut avcc = Vec::with_capacity(access_unit.bytes.len());
        for nal in nals {
            let len = u32::try_from(nal.len())
                .map_err(|_| DecodeError::InvalidBitstream("AVCC NAL is too large"))?;
            avcc.extend_from_slice(&len.to_be_bytes());
            avcc.extend_from_slice(nal);
        }
        Ok(VideoToolboxSample {
            avcc,
            sps: self.sps.clone(),
            pps: self.pps.clone(),
            configuration_changed: changed,
        })
    }
}

fn parse_annex_b(bytes: &[u8]) -> Result<Vec<&[u8]>, DecodeError> {
    let mut nals = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if nals.len() >= MAX_NAL_UNITS {
            return Err(DecodeError::InvalidBitstream("NAL count exceeds bound"));
        }
        let (start, prefix) = find_start_code(bytes, offset)
            .ok_or(DecodeError::InvalidBitstream("missing Annex B start code"))?;
        if bytes[offset..start].iter().any(|byte| *byte != 0) {
            return Err(DecodeError::InvalidBitstream(
                "nonzero bytes precede Annex B NAL",
            ));
        }
        let nal_start = start
            .checked_add(prefix)
            .ok_or(DecodeError::InvalidBitstream("NAL offset overflow"))?;
        let (end, _) = find_start_code(bytes, nal_start).unwrap_or((bytes.len(), 0));
        let nal = bytes
            .get(nal_start..end)
            .ok_or(DecodeError::InvalidBitstream("NAL bounds are invalid"))?;
        if nal.is_empty() || nal[0] & 0x80 != 0 || nal[0] & 0x1f == 0 {
            return Err(DecodeError::InvalidBitstream("invalid H.264 NAL header"));
        }
        nals.push(nal);
        offset = end;
    }
    if nals.is_empty() {
        return Err(DecodeError::InvalidBitstream(
            "access unit has no NAL units",
        ));
    }
    Ok(nals)
}

fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index + 3 <= bytes.len() {
        if bytes.get(index..index + 3) == Some(&[0, 0, 1]) {
            return Some((index, 3));
        }
        if index + 4 <= bytes.len() && bytes.get(index..index + 4) == Some(&[0, 0, 0, 1]) {
            return Some((index, 4));
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(epoch: u16, frame: u32, key: bool, config: bool, bytes: &[u8]) -> EncodedAccessUnit {
        EncodedAccessUnit::new(epoch, frame, frame, key, config, bytes.to_vec()).unwrap()
    }

    #[test]
    fn converts_start_codes_and_caches_parameter_sets_for_following_samples() {
        let mut converter = VideoToolboxBitstream::new();
        let idr = access(
            1,
            1,
            true,
            true,
            &[
                0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 1, 0x68, 0xee, 0, 0, 0, 1, 0x65, 0x88,
            ],
        );
        let sample = converter.sample(&idr).unwrap();
        assert!(sample.configuration_changed);
        assert!(sample.sps.is_some() && sample.pps.is_some());
        assert_eq!(sample.avcc[0..4], [0, 0, 0, 4]);
        let p = access(1, 2, false, false, &[0, 0, 0, 1, 0x41, 0x9a]);
        let following = converter.sample(&p).unwrap();
        assert!(!following.configuration_changed);
        assert_eq!(following.avcc, [0, 0, 0, 2, 0x41, 0x9a]);
        assert_eq!(following.sps, sample.sps);
    }

    #[test]
    fn changed_parameter_sets_are_reported_for_decoder_reconfiguration() {
        let mut converter = VideoToolboxBitstream::new();
        converter
            .sample(&access(
                0,
                1,
                true,
                true,
                &[
                    0, 0, 0, 1, 0x67, 0x11, 0, 0, 0, 1, 0x68, 0x22, 0, 0, 0, 1, 0x65, 0x33,
                ],
            ))
            .unwrap();
        let changed = converter
            .sample(&access(
                0,
                2,
                false,
                true,
                &[
                    0, 0, 0, 1, 0x67, 0x44, 0, 0, 1, 0x68, 0x55, 0, 0, 0, 1, 0x41, 0x66,
                ],
            ))
            .unwrap();
        assert!(changed.configuration_changed);
        assert_eq!(changed.sps, Some(vec![0x67, 0x44]));
        assert_eq!(changed.pps, Some(vec![0x68, 0x55]));
    }

    #[test]
    fn idr_without_parameter_sets_is_rejected_until_configuration_arrives() {
        let mut converter = VideoToolboxBitstream::new();
        assert_eq!(
            converter.sample(&access(0, 1, true, false, &[0, 0, 0, 1, 0x65, 0x88])),
            Err(DecodeError::MissingCodecConfiguration)
        );
    }

    #[test]
    fn malformed_annex_b_is_rejected() {
        let mut converter = VideoToolboxBitstream::new();
        let mut unit = access(0, 1, false, false, &[0, 0, 0, 1, 0x41, 0x88]);
        unit.bytes = vec![0x41, 0x88];
        assert!(converter.sample(&unit).is_err());
    }
}
