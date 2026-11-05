//! One HTTP client builder that can trust an extra (private) CA.
//!
//! WHY THIS EXISTS, and it is not a nicety. reqwest is built here with
//! `rustls-tls`, which pulls **webpki-roots**: Mozilla's root set compiled *into
//! the binary*. It does not read the system trust store, and it does not read
//! `SSL_CERT_FILE` - that is an OpenSSL convention. So a private CA is invisible
//! to this process no matter what is mounted into the container.
//!
//! A deployment whose OIDC provider (or any other outbound dependency: Last.fm
//! and ListenBrainz behind a proxy, ntfy) presents a certificate signed by a
//! private CA would otherwise fail at startup with `Init("Request failed")`,
//! because OIDC discovery cannot verify the issuer's certificate.
//!
//! So this reads `SSL_CERT_FILE` - the variable most tooling already honours -
//! and adds those certificates as extra roots. Public roots still work - this
//! ADDS to webpki's set rather than replacing it, so public hosts are unaffected.

use std::path::PathBuf;

/// Where to look for extra roots: a PEM file or bundle, the OpenSSL convention.
const CERT_FILE_ENV: &str = "SSL_CERT_FILE";

/// A `reqwest::ClientBuilder` that trusts the internal CA when one is configured.
///
/// Use this instead of `reqwest::Client::builder()` anywhere a request may reach
/// an internal host. A missing or unreadable file is not fatal: the builder is
/// returned with public roots only, because a service that refuses to start when
/// a CA file is absent cannot run where no private CA exists - and the failure it would
/// produce reads as "the network is broken" rather than "the CA is missing".
pub fn client_builder() -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder();
    for cert in extra_roots() {
        builder = builder.add_root_certificate(cert);
    }
    builder
}

/// Parse every certificate out of the bundle named by `SSL_CERT_FILE`.
///
/// A bundle, not a single certificate: a typical combined bundle is a private
/// CA concatenated with the public roots, and `Certificate::from_pem`
/// reads only the FIRST entry in a file. Taking just the first would work by luck
/// or fail silently depending on concatenation order, so each PEM block is
/// extracted and added individually.
fn extra_roots() -> Vec<reqwest::Certificate> {
    roots_from(std::env::var_os(CERT_FILE_ENV).map(PathBuf::from))
}

/// The path is a PARAMETER rather than read from the environment here, so the
/// behaviour can be tested without mutating process env. `set_var` is `unsafe` in
/// this edition (and the workspace denies unsafe), and env mutation races across
/// parallel tests in the same binary - two reasons to keep it at the edge.
fn roots_from(path: Option<PathBuf>) -> Vec<reqwest::Certificate> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        // Deliberately quiet: on a machine with no internal CA this is the normal
        // case, not a problem worth a warning on every start.
        return Vec::new();
    };
    pem_blocks(&bytes)
        .into_iter()
        .filter_map(|block| reqwest::Certificate::from_pem(&block).ok())
        .collect()
}

/// Split a PEM bundle into individual certificate blocks.
fn pem_blocks(bytes: &[u8]) -> Vec<Vec<u8>> {
    const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
    const END: &[u8] = b"-----END CERTIFICATE-----";

    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some(start) = find(rest, BEGIN) {
        let after = &rest[start..];
        let Some(end) = find(after, END) else { break };
        let stop = end + END.len();
        out.push(after[..stop].to_vec());
        rest = &after[stop..];
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const ONE: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

    #[test]
    fn splits_a_bundle_into_every_certificate() {
        // The whole point: a combined bundle holds a private CA
        // concatenated with public roots, and from_pem reads only the first.
        let bundle = format!("{ONE}{ONE}{ONE}");
        assert_eq!(pem_blocks(bundle.as_bytes()).len(), 3);
    }

    #[test]
    fn tolerates_comments_and_blank_lines_between_blocks() {
        // Real bundles carry human-readable subject lines between certificates.
        let bundle = format!("# Example Private CA\n{ONE}\n\n# ISRG Root X1\n{ONE}");
        assert_eq!(pem_blocks(bundle.as_bytes()).len(), 2);
    }

    #[test]
    fn a_truncated_final_block_is_dropped_not_returned_half_parsed() {
        let bundle = format!("{ONE}-----BEGIN CERTIFICATE-----\nBBBB\n");
        assert_eq!(pem_blocks(bundle.as_bytes()).len(), 1);
    }

    #[test]
    fn empty_and_garbage_input_yield_nothing() {
        assert!(pem_blocks(b"").is_empty());
        assert!(pem_blocks(b"not a certificate at all").is_empty());
    }

    #[test]
    fn no_configured_path_means_public_roots_only_rather_than_a_failure() {
        // A service that refuses to start without a CA file cannot run without
        // one, and the error would read as a network fault.
        assert!(roots_from(None).is_empty());
    }

    #[test]
    fn an_unreadable_path_is_survivable() {
        assert!(roots_from(Some(PathBuf::from("/nonexistent/ca-bundle.crt"))).is_empty());
    }

    #[test]
    fn every_certificate_in_a_file_is_loaded_not_just_the_first() {
        // THE bug this module exists to prevent. reqwest's Certificate::from_pem
        // reads only the FIRST entry in a file, and a combined bundle is a
        // private CA concatenated with public roots - so taking just the first
        // would trust the internal CA or the public ones depending on
        // concatenation order, and silently fail for whichever lost.
        //
        // (An earlier version of this test asserted these blocks were REJECTED as
        // invalid DER. They are not: rustls defers parsing, so from_pem accepts
        // them. Asserting the count is both true and the property that matters.)
        let dir = std::env::temp_dir().join("mh-tls-test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bundle.crt");
        std::fs::write(&f, format!("{ONE}{ONE}{ONE}")).unwrap();
        assert_eq!(roots_from(Some(f.clone())).len(), 3);
        let _ = std::fs::remove_file(f);
    }
}
