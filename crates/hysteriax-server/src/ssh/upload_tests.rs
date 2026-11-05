use super::*;
#[test]
fn upload_metadata_binds_destination_size_and_hash_without_payload() {
    let transfer = Transfer::new("/tmp/a'b", b"private file contents", 120).unwrap();
    assert_eq!(transfer.verification["remote_path"], "/tmp/a'b");
    assert_eq!(transfer.verification["content_bytes"], 21);
    assert_eq!(
        transfer.verification["content_sha256"],
        hex::encode(Sha256::digest(b"private file contents"))
    );
    assert!(!transfer.command.contains("private file contents"));
    assert!(Transfer::new("relative", b"", 120).is_err());
    assert!(Transfer::new("/tmp/a\0b", b"", 120).is_err());
    assert!(Transfer::new("/tmp/a", b"", 0).is_err());
}
