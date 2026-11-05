use super::*;
#[test]
fn allocation_targets_reject_local_and_documentation_addresses() {
    for value in [
        "127.0.0.1",
        "10.0.0.1",
        "192.0.2.1",
        "100.64.0.1",
        "198.18.0.1",
        "192.0.0.1",
        "::ffff:8.8.8.8",
        "fec0::1",
        "::1",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
    ] {
        assert!(!public_ip(value));
    }
    assert!(public_ip("8.8.8.8"));
    assert!(public_ip("2606:4700:4700::1111"));
}
