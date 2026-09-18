#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use paper_packages::archive::{ArchiveLimits, extract_tree};

// `extract_tree`'s own doc calls extraction "the hostile part": tar
// traversal, symlink escape, decompression bombs and malformed gzip all
// arrive as raw bytes on this path before anything trusts them (archive.rs).
// `ArchiveLimits::DEFAULT` is the same bound the installer uses, so a bomb is
// expected to fail bounded, not to allocate without limit.
fuzz_target!(|data: &[u8]| {
    let destination = tempfile::tempdir().expect("create temp destination");
    let _ = extract_tree(
        Cursor::new(data),
        destination.path(),
        ArchiveLimits::DEFAULT,
    );
});
