use super::*;
#[test]
fn record_validation_is_zone_scoped_and_normalizes_addresses() {
    let input = RecordInput {
        name: "HK.Example.COM.".into(),
        record_type: "aaaa".into(),
        content: "2001:0db8::1".into(),
        ttl: 300,
        proxied: false,
    };
    let normalized = input.normalize("example.com").unwrap();
    assert_eq!(normalized.name, "hk.example.com");
    assert_eq!(normalized.content, "2001:db8::1");
    assert!(
        RecordInput {
            name: "evil-example.com".into(),
            ..normalized.clone()
        }
        .normalize("example.com")
        .is_err()
    );
    assert!(
        RecordInput {
            record_type: "TXT".into(),
            ..normalized
        }
        .normalize("example.com")
        .is_err()
    );
    assert!(hostname("a..example.com").is_err());
}
