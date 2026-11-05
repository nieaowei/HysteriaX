use super::SecretBox;

#[test]
fn encrypts_and_authenticates_secret_values() {
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    let first = secrets.encrypt("example-private-key").unwrap();
    let second = secrets.encrypt("example-private-key").unwrap();
    assert_ne!(first, second);
    assert_eq!(secrets.decrypt(&first).unwrap(), "example-private-key");
    assert!(secrets.decrypt("invalid-ciphertext").is_err());

    let binary = [0_u8, 255, 64, 13, 10];
    let encrypted = secrets.encrypt_bytes(&binary).unwrap();
    assert_eq!(secrets.decrypt_bytes(&encrypted).unwrap(), binary);
}
