// Shared helpers for the integration tests. Included via `mod dns_utils;`
// in each consuming test crate (and compiled into them through Bazel's
// `shared_srcs`). `allow(dead_code)` because not every crate that includes
// this module uses every helper, and the standalone build sees none of them.
#![allow(dead_code, unreachable_pub)]

// ginepro's default resolver (hickory-dns) reads /etc/resolv.conf, which
// doesn't exist in sandboxed environments (e.g. Nix builds).
#[cfg(unix)]
pub fn dns_configured() -> bool {
    std::path::Path::new("/etc/resolv.conf").exists()
}
#[cfg(not(unix))]
pub const fn dns_configured() -> bool {
    true
}
