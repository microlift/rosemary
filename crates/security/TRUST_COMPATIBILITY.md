# Security.framework trust compatibility (limited subset)

This Rust implementation preserves the Security.framework C entry-point ABI for
`SecPolicyCreateSSL(Boolean, CFStringRef)` and
`SecTrustEvaluateWithError(SecTrustRef, CFErrorRef *)`, alongside existing legacy
trust interfaces. It is **not a replacement for macOS trustd or the system
Keychain**. An externally signed leaf is accepted only after `rustls-webpki`
verifies its signature chain, validity times, intended TLS usage (server or
client), and the requested server DNS/IP name when one is supplied.

## What is implemented

- A basic X.509 or SSL certificate policy, single-policy trust creation,
  legacy and modern evaluations, an optional explicit verification date, and
  caller-supplied trust anchors.
- The default trust roots are parsed from the host's PEM CA bundle (including
  `SSL_CERT_FILE` if present) and are the *same roots* returned by
  `SecTrustCopyAnchorCertificates`. A missing/unparseable host bundle results
  in zero default trust anchors, never implicit self-signed trust.
- By Apple's contract, `SecTrustSetAnchorCertificates` with a non-null array
  replaces the default set. `SecTrustSetAnchorCertificatesOnly(false)` combines
  custom and host roots, while a null `SecTrustSetAnchorCertificates` argument
  restores defaults. `SecTrustSetAnchorCertificatesOnly(true)` **never** includes
  host roots, even when no custom anchors have been supplied or after a null
  anchor reset; in that case the allowed set is empty and trust evaluation
  fails. An explicitly empty array does **not** restore defaults.
- Malformed certificates, untrusted chains, invalid signatures, wrong
  hostnames, incorrect TLS extended-key-usage, and expired/not-yet-valid
  certificates fail trust evaluation. `SecTrustEvaluateWithError` returns a
  retained `CFError`-compatible Foundation error on evaluation failure.

## Deliberate fail-closed limitations

- **OCSP:** No stapled OCSP response may produce trusted output. OCSP proof
  verification must check DER, response signature, authorized responder,
  certificate ID, status, response time and revocation time. Until that is
  implemented, passing *any non-null* response to `SecTrustSetOCSPResponse`
  poisons the individual trust object (including if a caller ignores the
  setter error). Properly typed nonempty responses return
  `errSecUnimplemented`; malformed input returns `errSecParam`. Null means no
  staple, **not** a positive revocation check. We do not fetch OCSP online,
  download missing intermediate certificates, or validate CRLs/CT/SCTs.
- **Trust options:** `SecTrustSetOptions` with any nonzero bit returns
  `errSecUnimplemented` and makes subsequent trust evaluation reject until the
  options are reset to zero. In particular, `RequireRevPerCert` must **not**
  silently downgrade to offline certificate-path validation, and expiration
  bypasses must not rewrite the verification clock.
- An SSL policy created with a null hostname follows Apple's API contract:
  there is no hostname to check. Callers protecting a remote TLS endpoint must
  supply its actual hostname themselves.
- Only one policy per trust object is implemented; compound policy arrays
  return `errSecParam` rather than silently dropping conditions. Trust
  settings and per-user Keychain exceptions on real macOS are not modeled.
- Apple-specific `CFError` subclass behavior, exact per-failure OSStatus codes,
  user Keychain trust settings, and AIA/OCSP network behavior are not fully
  emulated. Certificate subject summaries are informational, not evidence.

The DER test fixtures contain a locally signed CA and server-only certificate
for `studio.example.test`, issued 2026-10-20, with a long validity interval.
No private keys are committed. Tests evaluate them at explicit fixed Unix
timestamps (both valid and invalid), never by contacting the network.

## Reference interfaces

- Apple `SecPolicyCreateSSL`: https://developer.apple.com/documentation/security/secpolicycreatessl(_:_:)
- Apple `SecTrustEvaluateWithError`: https://developer.apple.com/documentation/security/sectrustevaluatewitherror(_:_:)
- Apple anchor-only behavior: https://developer.apple.com/documentation/security/sectrustsetanchorcertificatesonly(_:_:)
- Apple OCSP response contract: https://developer.apple.com/documentation/security/sectrustsetocspresponse(_:_:)
- Apple trust option flags: https://developer.apple.com/documentation/security/sectrustoptionflags
- rustls-webpki verification API: https://docs.rs/rustls-webpki/latest/webpki/struct.EndEntityCert.html
