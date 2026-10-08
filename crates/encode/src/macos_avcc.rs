//! VideoToolbox AVCC access-unit normalization used by the macOS H.264 adapter.
//!
//! This parser is platform-neutral so bounds and parameter-set insertion are covered on all hosts.

use crate::{EncodeError, EncodedPacket, MAX_ENCODED_PACKET_BYTES};

const MAX_NAL_UNITS: usize = 4096;

/// Converts one length-prefixed VideoToolbox sample to Annex B.
///
/// `cached_sps` and `cached_pps` are parameter sets obtained from the current VideoToolbox format
/// description. On IDR frames, missing parameter sets are inserted before the first slice so
/// every independently decodable keyframe carries its configuration.
pub fn videotoolbox_avcc_to_annex_b(
    input: &[u8],
    length_size: usize,
    timestamp_us: u64,
    cached_sps: Option<&[u8]>,
    cached_pps: Option<&[u8]>,
) -> Result<(EncodedPacket, bool), EncodeError> {
    if input.is_empty() {
        return Err(EncodeError::InvalidBitstream(
            "empty VideoToolbox sample".to_owned(),
        ));
    }
    if !(1..=4).contains(&length_size) {
        return Err(EncodeError::InvalidBitstream(
            "AVCC NAL length size must be 1 through 4".to_owned(),
        ));
    }
    let mut nals: Vec<&[u8]> = Vec::new();
    let mut offset = 0usize;
    while offset < input.len() {
        if nals.len() >= MAX_NAL_UNITS {
            return Err(EncodeError::InvalidBitstream(
                "NAL unit count exceeds the configured bound".to_owned(),
            ));
        }
        let length_end = offset
            .checked_add(length_size)
            .ok_or_else(|| EncodeError::InvalidBitstream("NAL length overflow".to_owned()))?;
        let encoded_length = input
            .get(offset..length_end)
            .ok_or_else(|| EncodeError::InvalidBitstream("truncated AVCC NAL length".to_owned()))?;
        let mut nal_len = 0usize;
        for byte in encoded_length {
            nal_len = nal_len
                .checked_mul(256)
                .and_then(|value| value.checked_add(usize::from(*byte)))
                .ok_or_else(|| EncodeError::InvalidBitstream("NAL length overflow".to_owned()))?;
        }
        if nal_len == 0 {
            return Err(EncodeError::InvalidBitstream(
                "zero-length AVCC NAL".to_owned(),
            ));
        }
        let nal_end = length_end
            .checked_add(nal_len)
            .ok_or_else(|| EncodeError::InvalidBitstream("NAL length overflow".to_owned()))?;
        let nal = input.get(length_end..nal_end).ok_or_else(|| {
            EncodeError::InvalidBitstream("truncated AVCC NAL payload".to_owned())
        })?;
        validate_nal(nal)?;
        nals.push(nal);
        offset = nal_end;
    }

    let idr_position = nals.iter().position(|nal| nal_type(nal) == 5);
    let keyframe = idr_position.is_some();
    let mut output = Vec::with_capacity(input.len().saturating_add(256));
    if let Some(idr_index) = idr_position {
        let before_idr = &nals[..idr_index];
        if !before_idr.iter().any(|nal| nal_type(nal) == 7) {
            let sps = nals
                .iter()
                .find(|nal| nal_type(nal) == 7)
                .copied()
                .or(cached_sps)
                .ok_or_else(|| {
                    EncodeError::InvalidBitstream("VideoToolbox IDR has no SPS".to_owned())
                })?;
            validate_expected_nal(sps, 7)?;
            append_nal(&mut output, sps)?;
        }
        if !before_idr.iter().any(|nal| nal_type(nal) == 8) {
            let pps = nals
                .iter()
                .find(|nal| nal_type(nal) == 8)
                .copied()
                .or(cached_pps)
                .ok_or_else(|| {
                    EncodeError::InvalidBitstream("VideoToolbox IDR has no PPS".to_owned())
                })?;
            validate_expected_nal(pps, 8)?;
            append_nal(&mut output, pps)?;
        }
    }
    for nal in nals {
        append_nal(&mut output, nal)?;
    }
    let (has_sps, has_pps) = output_nal_types(&output)?;
    let has_config = has_sps && has_pps;
    Ok((
        EncodedPacket {
            bytes: output,
            timestamp_us,
            keyframe,
        },
        has_config,
    ))
}
fn validate_nal(nal: &[u8]) -> Result<(), EncodeError> {
    if nal.is_empty() || nal[0] & 0x80 != 0 || nal_type(nal) == 0 || nal_type(nal) > 23 {
        return Err(EncodeError::InvalidBitstream(
            "invalid H.264 NAL header".to_owned(),
        ));
    }
    Ok(())
}

fn validate_expected_nal(nal: &[u8], expected: u8) -> Result<(), EncodeError> {
    validate_nal(nal)?;
    if nal_type(nal) != expected {
        return Err(EncodeError::InvalidBitstream(
            "VideoToolbox parameter-set type mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |header| header & 0x1f)
}

fn append_nal(output: &mut Vec<u8>, nal: &[u8]) -> Result<(), EncodeError> {
    let total = output
        .len()
        .checked_add(4)
        .and_then(|len| len.checked_add(nal.len()))
        .ok_or_else(|| {
            EncodeError::InvalidBitstream("Annex B output length overflow".to_owned())
        })?;
    if total > MAX_ENCODED_PACKET_BYTES {
        return Err(EncodeError::InvalidBitstream(
            "Annex B access unit exceeds the configured bound".to_owned(),
        ));
    }
    output.extend_from_slice(&[0, 0, 0, 1]);
    output.extend_from_slice(nal);
    Ok(())
}

fn output_nal_types(bytes: &[u8]) -> Result<(bool, bool), EncodeError> {
    let mut has_sps = false;
    let mut has_pps = false;
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes.get(offset..offset + 4) != Some(&[0, 0, 0, 1]) {
            return Err(EncodeError::InvalidBitstream(
                "internal Annex B start-code error".to_owned(),
            ));
        }
        let start = offset + 4;
        let next = bytes
            .get(start..)
            .and_then(|tail| tail.windows(4).position(|window| window == [0, 0, 0, 1]))
            .map(|n| start + n)
            .unwrap_or(bytes.len());
        let nal = bytes.get(start..next).ok_or_else(|| {
            EncodeError::InvalidBitstream("internal Annex B NAL bounds error".to_owned())
        })?;
        if nal.is_empty() {
            return Err(EncodeError::InvalidBitstream("empty output NAL".to_owned()));
        }
        has_sps |= nal_type(nal) == 7;
        has_pps |= nal_type(nal) == 8;
        offset = next;
    }
    Ok((has_sps, has_pps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annex(nals: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for nal in nals {
            bytes.extend_from_slice(&[0, 0, 0, 1]);
            bytes.extend_from_slice(nal);
        }
        bytes
    }
    fn avcc(nals: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for nal in nals {
            bytes.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            bytes.extend_from_slice(nal);
        }
        bytes
    }

    #[test]
    fn idr_inserts_cached_parameter_sets_before_slice() {
        let sps = [0x67, 0x64, 0x00, 0x1f];
        let pps = [0x68, 0xee, 0x3c, 0x80];
        let idr = [0x65, 0x88, 0x84];
        let (packet, has_config) =
            videotoolbox_avcc_to_annex_b(&avcc(&[&idr]), 4, 77, Some(&sps), Some(&pps)).unwrap();
        assert!(packet.keyframe);
        assert!(has_config);
        assert_eq!(packet.timestamp_us, 77);
        assert_eq!(packet.bytes, annex(&[&sps, &pps, &idr]));
    }

    #[test]
    fn in_band_parameter_sets_are_not_duplicated() {
        let sps = [0x67, 0x64];
        let pps = [0x68, 0xee];
        let idr = [0x65, 0x88];
        let (packet, config) =
            videotoolbox_avcc_to_annex_b(&avcc(&[&sps, &pps, &idr]), 4, 0, None, None).unwrap();
        assert!(packet.keyframe && config);
        assert_eq!(packet.bytes, annex(&[&sps, &pps, &idr]));
    }

    #[test]
    fn idr_gets_parameter_sets_before_it_even_when_in_band_sets_follow_it() {
        let sps = [0x67, 0x64];
        let pps = [0x68, 0xee];
        let idr = [0x65, 0x88];
        let (packet, config) =
            videotoolbox_avcc_to_annex_b(&avcc(&[&idr, &sps, &pps]), 4, 0, None, None).unwrap();
        assert!(packet.keyframe && config);
        assert_eq!(packet.bytes, annex(&[&sps, &pps, &idr, &sps, &pps]));
    }
    #[test]
    fn non_idr_never_injects_or_claims_missing_configuration() {
        let p = [0x41, 0x9a];
        let (packet, config) =
            videotoolbox_avcc_to_annex_b(&avcc(&[&p]), 4, 1, None, None).unwrap();
        assert!(!packet.keyframe && !config);
    }

    #[test]
    fn rejects_truncated_or_invalid_avcc_and_missing_idr_configuration() {
        assert!(videotoolbox_avcc_to_annex_b(&[0, 0], 4, 0, None, None).is_err());
        assert!(videotoolbox_avcc_to_annex_b(&[0, 0, 0, 2, 0x65], 4, 0, None, None).is_err());
        assert!(videotoolbox_avcc_to_annex_b(&avcc(&[&[0x65, 0x88]]), 4, 0, None, None).is_err());
        assert!(videotoolbox_avcc_to_annex_b(&avcc(&[&[0x41]]), 0, 0, None, None).is_err());
    }
}
