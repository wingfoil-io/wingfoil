// Compatibility shim: re-exports hickory-resolver as trust-dns-resolver.
// hickory-resolver is the successor to trust-dns-resolver and uses idna >= 1.0.3,
// fixing RUSTSEC-2024-0421 (idna accepts Punycode labels that produce no non-ASCII output).
pub use hickory_resolver::*;
