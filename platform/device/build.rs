//! Builds the native bridge, and only when asked to.
//!
//! Without the `vendor-engine` feature this does nothing at all: a Mac build of
//! `paper-device` compiles no C++, needs no Qt and links no vendor library.
//! That is what lets the portable half of the adapter — geometry, waveform
//! selection, evdev decoding, coordinate transforms — be developed and tested
//! on the Mac while the tablet is somewhere else.
//!
//! With the feature on, three things must be present, and the build says
//! plainly which one is missing rather than producing a linker error:
//!
//! | Variable | What it points at |
//! |---|---|
//! | `PAPERCLIP_QT_INCLUDE` | the SDK's Qt 6.10.3 headers |
//! | `PAPERCLIP_SYSROOT` | the reMarkable SDK sysroot (optional, for `--sysroot`) |
//! | `PAPERCLIP_VENDOR_LIB_DIR` | a directory holding `libqsgepaper.so` |
//!
//! `libqsgepaper.so` is proprietary (`LICENSE: CLOSED`). It is copied off the
//! tablet into a directory outside this repository and pointed at; it is never
//! committed. See `native/README.md`.

/// Where the vendor scenegraph plugin lives on the tablet.
///
/// Repeated in `tools/paperctl/build.rs`, because `cargo::rustc-link-arg` only
/// reaches the targets of the package that emits it.
#[cfg(feature = "vendor-engine")]
const VENDOR_PLUGIN_DIR: &str = "/usr/lib/plugins/scenegraph";

fn main() {
    println!("cargo::rerun-if-changed=native/paperclip_ep.cpp");
    println!("cargo::rerun-if-changed=native/paperclip_ep.h");
    println!("cargo::rerun-if-changed=native/ep_abi.hpp");
    println!("cargo::rerun-if-env-changed=PAPERCLIP_QT_INCLUDE");
    println!("cargo::rerun-if-env-changed=PAPERCLIP_SYSROOT");
    println!("cargo::rerun-if-env-changed=PAPERCLIP_VENDOR_LIB_DIR");

    #[cfg(feature = "vendor-engine")]
    build_bridge();
}

#[cfg(feature = "vendor-engine")]
fn build_bridge() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("linux") || !target.starts_with("aarch64") {
        panic!(
            "the `vendor-engine` feature targets the tablet: expected an \
             aarch64 Linux target, got `{target}`. Build with \
             `--target aarch64-unknown-linux-gnu`, or drop the feature."
        );
    }

    let qt_include = require(
        "PAPERCLIP_QT_INCLUDE",
        "the reMarkable SDK's Qt 6.10.3 include directory",
    );
    let vendor_lib_dir = require(
        "PAPERCLIP_VENDOR_LIB_DIR",
        "a directory holding libqsgepaper.so copied off the tablet",
    );

    let library = std::path::Path::new(&vendor_lib_dir).join("libqsgepaper.so");
    if !library.exists() {
        panic!(
            "PAPERCLIP_VENDOR_LIB_DIR is `{vendor_lib_dir}` but there is no \
             libqsgepaper.so in it. Copy it off the tablet read-only:\n  \
             scp remarkable-wifi:/usr/lib/plugins/scenegraph/libqsgepaper.so {vendor_lib_dir}/\n\
             It is proprietary and must not be committed."
        );
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("native/paperclip_ep.cpp")
        .include("native")
        .include(&qt_include)
        .include(format!("{qt_include}/QtCore"))
        .include(format!("{qt_include}/QtGui"))
        // A C++ exception reaching Rust is undefined behaviour, and this
        // bridge's whole job is that none does. Exceptions stay enabled —
        // `-fno-exceptions` would turn a caught vendor throw into a
        // `std::terminate` — but every entry point catches `...`.
        .flag_if_supported("-fvisibility=hidden")
        .warnings(true);

    if let Ok(sysroot) = std::env::var("PAPERCLIP_SYSROOT") {
        build.flag(format!("--sysroot={sysroot}"));
        println!("cargo::rustc-link-arg=--sysroot={sysroot}");
    }

    build.compile("paperclip_ep");

    println!("cargo::rustc-link-search=native={vendor_lib_dir}");
    // `libqsgepaper.so` needs Qt Quick and `libQt6Gui.so` needs Qt DBus, and
    // GNU ld chases those transitively at link time even though nothing here
    // calls them. On the tablet they are all present; in a build environment
    // holding only the libraries we actually link against, they are not.
    // Leaving them unresolved is correct rather than lax — the dynamic linker
    // resolves them on the device, and pulling them in explicitly would add
    // DT_NEEDED entries for libraries Paperclip does not use.
    println!("cargo::rustc-link-arg=-Wl,--allow-shlib-undefined");
    // `libqsgepaper.so` lives in Qt's scenegraph plugin directory, which is not
    // on the device's loader path: Xochitl finds it as a *plugin*, by asking
    // Qt, and a plain ELF that merely links it does not. Without an RPATH the
    // binary dies at exec with "cannot open shared object file" and the caller
    // has to remember an LD_LIBRARY_PATH — which is not the single command §4
    // asks for. Confirmed on the device, image 20260827113527.
    println!("cargo::rustc-link-arg=-Wl,-rpath,{VENDOR_PLUGIN_DIR}");
    println!("cargo::rustc-link-lib=dylib=qsgepaper");
    println!("cargo::rustc-link-lib=dylib=Qt6Core");
    println!("cargo::rustc-link-lib=dylib=Qt6Gui");
    println!("cargo::rustc-link-lib=dylib=stdc++");
}

#[cfg(feature = "vendor-engine")]
fn require(name: &str, what: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "the `vendor-engine` feature needs {name} set to {what}. \
             See platform/device/native/README.md."
        )
    })
}
