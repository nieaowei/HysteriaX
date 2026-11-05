use super::{normalize_arch, supported_release};

#[test]
fn clear_response_requires_success_and_complete_unambiguous_framing() {
    use super::parse_complete_http_response as parse;
    assert_eq!(
        parse(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(),
        b"{}"
    );
    assert_eq!(
        parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n")
            .unwrap(),
        b"{}"
    );
    for response in [
        &b"HTTP/1.1 500 Error\r\nContent-Length: 2\r\n\r\n{}"[..],
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}XX0\r\n\r\n",
    ] {
        assert!(
            parse(response).is_err(),
            "accepted {:?}",
            String::from_utf8_lossy(response)
        );
    }
}

#[test]
fn normalizes_supported_linux_architectures() {
    assert_eq!(normalize_arch("x86_64"), "amd64");
    assert_eq!(normalize_arch("aarch64"), "arm64");
}

#[test]
fn client_prefers_x25519_without_removing_supported_exchanges() {
    let configured = super::client_config();
    let defaults = russh::client::Config::default();
    assert_eq!(
        configured.preferred.kex.first(),
        Some(&russh::kex::CURVE25519)
    );
    assert_eq!(configured.preferred.kex.len(), defaults.preferred.kex.len());
    for algorithm in defaults.preferred.kex.iter() {
        assert!(configured.preferred.kex.contains(algorithm));
    }
    assert!(configured.nodelay);
}

#[test]
fn accepts_only_the_pinned_os_matrix() {
    assert!(supported_release("debian", "12"));
    assert!(supported_release("debian", "13"));
    assert!(supported_release("ubuntu", "22.04"));
    assert!(supported_release("ubuntu", "24.04"));
    assert!(!supported_release("ubuntu", "25.04"));
    assert!(!supported_release("centos", "9"));
}
