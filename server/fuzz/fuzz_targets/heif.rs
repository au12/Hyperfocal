//! Fuzz target: HEIF/HEIC metadata and image items (see hyperfocal::fuzzing::heif).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| hyperfocal::fuzzing::heif(data));
