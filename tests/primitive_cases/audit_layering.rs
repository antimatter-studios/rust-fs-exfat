#[test]
fn allocation_audits_delegate_byte_decoding_and_stream_walks() {
    let source = include_str!("../../src/check_audit.rs");
    for local_decode in [
        "::from_le_bytes",
        "primary[1]",
        "self.within",
        "bytes[index / 8]",
        "bitmap.last_mut()",
        "boot.volume_flags &",
    ] {
        assert!(
            !source.contains(local_decode),
            "check_audit.rs still decodes or walks on-disk structures: {local_decode}"
        );
    }
}
