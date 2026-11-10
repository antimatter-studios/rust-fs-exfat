#[test]
fn strict_boot_parsing_delegates_byte_decoding_and_checksums_to_primitives() {
    let source = include_str!("../../src/boot.rs");
    for duplicate in ["::from_le_bytes", "rotate_right(", "prefix[108]"] {
        assert!(
            !source.contains(duplicate),
            "boot.rs still decodes disk bytes instead of calling primitives: {duplicate}"
        );
    }
}
