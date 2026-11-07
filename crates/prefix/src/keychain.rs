//! The system keychains, as the files a Mac keeps them in.
//!
//! Programs that run their own TLS stack - Zig's `std.crypto`, for one - find
//! the trusted roots by reading these files directly rather than asking
//! Security.framework. MiniDarwin ships neither, so the namespace mounts files
//! written here: the system roots are the host's CA bundle, the same set
//! `security`'s trust settings report as the system domain.

use std::sync::OnceLock;

/// The read-only keychain holding the roots the system trusts.
pub const SYSTEM_ROOTS: &str = "/System/Library/Keychains/SystemRootCertificates.keychain";

/// The administrator's keychain, empty here.
pub const SYSTEM: &str = "/Library/Keychains/System.keychain";

/// `CSSM_DL_DB_RECORD_X509_CERTIFICATE`.
const X509_CERTIFICATE_TABLE: u32 = 0x8000_1000;

/// The `kych` file format's version 1.0.
const FORMAT_VERSION: u32 = 0x0001_0000;

/// The five 32-bit fields of the file header.
const HEADER_SIZE: u32 = 5 * 4;

/// A table header's seven 32-bit fields, before its record offsets.
const TABLE_HEADER_SIZE: u32 = 7 * 4;

/// A record's six fixed fields and its nine attribute offsets (certificate
/// type, encoding, print name, alias, subject, issuer, serial number, subject
/// key identifier and public key hash), before its data.
const CERTIFICATE_RECORD_HEADER_SIZE: u32 = 15 * 4;

/// The host's root certificates, DER-encoded, read once.
///
/// From the bundle the host's own TLS libraries trust, so a program running
/// its own stack trusts the same servers the rest of the machine does.
/// `SSL_CERT_FILE` first, as OpenSSL reads it, then where Debian, Fedora,
/// NixOS and the BSDs keep theirs.
pub fn host_anchors() -> &'static [Vec<u8>] {
    static ANCHORS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    ANCHORS.get_or_init(|| {
        use rustls_pki_types::CertificateDer;
        use rustls_pki_types::pem::PemObject;
        const BUNDLES: &[&str] = &[
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/ssl/cert.pem",
            "/usr/local/share/certs/ca-root-nss.crt",
        ];
        let named = std::env::var_os("SSL_CERT_FILE").map(std::path::PathBuf::from);
        let pem = named
            .into_iter()
            .chain(BUNDLES.iter().map(std::path::PathBuf::from))
            .find_map(|path| std::fs::read(path).ok())
            .unwrap_or_default();
        CertificateDer::pem_slice_iter(&pem)
            .filter_map(Result::ok)
            .map(|der| der.to_vec())
            .collect()
    })
}

/// A keychain file holding `certificates`, DER-encoded, and nothing else.
///
/// FIXME (ACCURACY): only the certificate table is written - no schema tables,
/// no attributes, no indexes. A reader that walks the certificate table finds
/// every certificate; Apple's own database layer would not open the file.
pub fn encode(certificates: &[Vec<u8>]) -> Vec<u8> {
    let mut records = Vec::new();
    let mut offsets = Vec::with_capacity(certificates.len());
    let records_start = TABLE_HEADER_SIZE + 4 * certificates.len() as u32;
    for (number, der) in certificates.iter().enumerate() {
        offsets.push(records_start + records.len() as u32);
        let size = (CERTIFICATE_RECORD_HEADER_SIZE as usize + der.len()).next_multiple_of(4);
        let record_start = records.len();
        push(&mut records, size as u32);
        push(&mut records, number as u32);
        // Create version, record version.
        push(&mut records, 0);
        push(&mut records, 0);
        push(&mut records, der.len() as u32);
        // Semantic information.
        push(&mut records, 0);
        // Every attribute absent.
        records.resize(record_start + CERTIFICATE_RECORD_HEADER_SIZE as usize, 0);
        records.extend_from_slice(der);
        records.resize(record_start + size, 0);
    }

    let count = certificates.len() as u32;
    let table_size = records_start + records.len() as u32;
    let mut table = Vec::with_capacity(table_size as usize);
    push(&mut table, table_size);
    push(&mut table, X509_CERTIFICATE_TABLE);
    push(&mut table, count);
    push(&mut table, records_start);
    // Indexes offset: the table ends where its indexes would start.
    push(&mut table, table_size);
    // Free list head: none.
    push(&mut table, 0);
    push(&mut table, count);
    for offset in offsets {
        push(&mut table, offset);
    }
    table.extend_from_slice(&records);

    // The schema: its size, one table, and that table's offset from it.
    let table_offset = 3 * 4;
    let mut file = Vec::with_capacity((HEADER_SIZE + table_offset + table_size) as usize);
    file.extend_from_slice(b"kych");
    push(&mut file, FORMAT_VERSION);
    push(&mut file, HEADER_SIZE);
    // Schema offset; then no authentication data.
    push(&mut file, HEADER_SIZE);
    push(&mut file, 0);
    push(&mut file, table_offset + table_size);
    push(&mut file, 1);
    push(&mut file, table_offset);
    file.extend_from_slice(&table);
    file
}

fn push(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// Walk the file the way a reader of the certificate table does.
    fn certificates(file: &[u8]) -> Vec<Vec<u8>> {
        assert_eq!(&file[..4], b"kych");
        let schema = word(file, 12) as usize;
        let tables = word(file, schema + 4) as usize;
        let mut out = Vec::new();
        for t in 0..tables {
            let table = schema + word(file, schema + 8 + 4 * t) as usize;
            if word(file, table + 4) != X509_CERTIFICATE_TABLE {
                continue;
            }
            for r in 0..word(file, table + 8) as usize {
                let record = table + word(file, table + 28 + 4 * r) as usize;
                assert_eq!(record % 4, 0);
                let size = word(file, record + 16) as usize;
                let data = record + CERTIFICATE_RECORD_HEADER_SIZE as usize;
                out.push(file[data..data + size].to_vec());
            }
        }
        out
    }

    #[test]
    fn certificate_table_holds_each_certificate() {
        let certificates_in = vec![vec![1, 2, 3], vec![4; 9], vec![]];
        let file = encode(&certificates_in);
        assert_eq!(certificates(&file), certificates_in);
    }

    #[test]
    fn empty_keychain_has_an_empty_certificate_table() {
        assert!(certificates(&encode(&[])).is_empty());
    }
}
