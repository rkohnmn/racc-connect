use crate::TopologyError;
use racc_proto::ProtoError;

#[test]
fn protocol_error_wrapper_preserves_the_proto_reason() {
    assert_eq!(
        TopologyError::from(ProtoError::Truncated),
        TopologyError::Protocol(ProtoError::Truncated)
    );
}
