//! Embeds the Windows application manifest into the executable.
//!
//! The manifest is what turns on Common Controls 6.0 (visual styles) and declares Per-Monitor V2
//! DPI awareness; see packaging/screen-memory.manifest for why that matters. Embedding goes
//! through the MSVC linker's own manifest tool rather than an extra crate, so there is nothing to
//! add to [build-dependencies].
//!
//! The manifest deliberately carries no <trustInfo>: the linker contributes the default
//! asInvoker fragment itself, and supplying both makes the merge fail.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    // /MANIFEST is a link.exe flag, so this only applies to the MSVC toolchain
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("packaging")
        .join("screen-memory.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
}
