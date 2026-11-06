//! The separate test binary initializes host roots with the deterministic
//! public test CA before `host_anchors()` is first queried. No host trust
//! store is changed; the environment setting belongs only to this process.
use security::trust::*;

#[test]
fn anchors_only_excludes_host_roots_even_without_custom_anchors() {
    // SAFETY: this integration test has one test case, and no concurrent
    // caller can race the process-local SSL_CERT_FILE setting.
    unsafe {
        std::env::set_var(
            "SSL_CERT_FILE",
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/tls_unit_ca.pem"),
        );
    }
    foundation::bootstrap();
    unsafe {
        let root_der = include_bytes!("../src/testdata/tls_unit_ca.der");
        let leaf_der = include_bytes!("../src/testdata/tls_unit_leaf.der");
        let data = corefoundation::api::CFDataCreate(
            std::ptr::null(),
            leaf_der.as_ptr(),
            leaf_der.len() as _,
        );
        let root_data = corefoundation::api::CFDataCreate(
            std::ptr::null(),
            root_der.as_ptr(),
            root_der.len() as _,
        );
        let cert = SecCertificateCreateWithData(std::ptr::null(), data);
        let root = SecCertificateCreateWithData(std::ptr::null(), root_data);
        let policy = SecPolicyCreateBasicX509();
        let mut trust = std::ptr::null();
        assert_eq!(SecTrustCreateWithCertificates(cert, policy, &mut trust), 0);
        let date = foundation::timer::date_since_1970(1_800_000_000.0);
        assert_eq!(SecTrustSetVerifyDate(trust, date as _), 0);
        let verify = |expected: u8| {
            let mut legacy = 0;
            assert_eq!(SecTrustEvaluate(trust, &mut legacy), 0);
            assert_eq!(
                SecTrustEvaluateWithError(trust, std::ptr::null_mut()),
                expected
            );
            assert_eq!(legacy == 4, expected == 1);
        };
        // The exact fixture CA is loaded as the host root.
        verify(1);
        assert_eq!(SecTrustSetAnchorCertificatesOnly(trust, 1), 0);
        verify(0); // No custom anchors must mean no roots, not host roots.
        assert_eq!(SecTrustSetAnchorCertificatesOnly(trust, 0), 0);
        verify(1);
        let roots = corefoundation::collections::new_array(
            &[root],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        assert_eq!(SecTrustSetAnchorCertificates(trust, roots), 0);
        verify(1); // The app's explicitly permitted root is usable.
        assert_eq!(SecTrustSetAnchorCertificates(trust, std::ptr::null()), 0);
        verify(1); // NULL restores defaults, with anchors-only false.
        assert_eq!(SecTrustSetAnchorCertificatesOnly(trust, 1), 0);
        verify(0); // Enabling only after NULL excludes host roots again.
        for object in [data, root_data, cert, root, policy, trust, roots, date as _] {
            corefoundation::api::CFRelease(object);
        }
    }
}
