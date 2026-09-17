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
//! at its final link with several hundred Qt symbols. So `paperctl` repeats it
//! here, under the same feature.
//!
//! This is the whole of `paperctl`'s knowledge of the vendor engine, and it is
//! deliberately a link flag rather than anything it can call: §8 keeps Qt types,
//! device paths and systemd behind `paper-device`, and nothing above that crate
//! may import them. A build script emitting a flag imports nothing.

fn main() {
    println!("cargo::rerun-if-env-changed=PAPERCLIP_VENDOR_LIB_DIR");

    #[cfg(feature = "vendor-engine")]
    println!("cargo::rustc-link-arg=-Wl,--allow-shlib-undefined");
}
