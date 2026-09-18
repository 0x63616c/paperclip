#![no_main]

use libfuzzer_sys::fuzz_target;
use paper_packages::Manifest;

// `Manifest::parse` is the door every `paper.toml` comes through, whether
// read off disk or fetched from a catalog over the network (manifest.rs).
// This target only has to not panic or hang; a hostile parse must become a
// `Result::Err`, never a crash.
fuzz_target!(|text: &str| {
    let _ = Manifest::parse(text);
});
