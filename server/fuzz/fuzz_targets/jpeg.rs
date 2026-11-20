//! Fuzz target: JPEG headers and image data (see hyperfocal::fuzzing::jpeg).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| hyperfocal::fuzzing::jpeg(data));
