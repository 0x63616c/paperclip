//! One linker flag, and the reason it cannot live where it belongs.
//!
//! `platform/device/build.rs` explains why linking `libqsgepaper.so` needs
//! `-Wl,--allow-shlib-undefined`: the vendor library pulls in Qt Quick, Qml and
//! DBus, GNU ld chases those transitively even though Paperclip calls none of
//! them, and a build machine holding only the libraries we actually link has no
//! answer for them. The tablet does, at load time.
//!
//! It emits that flag with `cargo::rustc-link-arg` — which Cargo applies to the
//! targets of *that package only*. `paper-device`'s own `takeover` example gets
//! it; a binary in another package linking the same library does not, and fails
//! at its final link with several hundred Qt symbols (confirmed building
//! `paperclip-compositor` under Docker with `vendor-engine`, WWW-86 — the same
//! failure `tools/paperctl/build.rs`'s own doc comment already recorded for
//! `paperctl`). So `paperclip-compositor` repeats it here, under the same
//! feature — this crate is the other binary that links the vendor engine
//! directly (ADR-0039: it is the one process that ever opens the panel).
//!
//! The RPATH is here for the same reason. `libqsgepaper.so` sits in Qt's
//! scenegraph *plugin* directory, which is not on the device's loader path;
//! without it the binary dies at exec.

/// Kept in step with `platform/device/build.rs`, which owns the explanation.
#[cfg(feature = "vendor-engine")]
const VENDOR_PLUGIN_DIR: &str = "/usr/lib/plugins/scenegraph";

fn main() {
    println!("cargo::rerun-if-env-changed=PAPERCLIP_VENDOR_LIB_DIR");

    #[cfg(feature = "vendor-engine")]
    {
        println!("cargo::rustc-link-arg=-Wl,--allow-shlib-undefined");
        println!("cargo::rustc-link-arg=-Wl,-rpath,{VENDOR_PLUGIN_DIR}");
    }
}
