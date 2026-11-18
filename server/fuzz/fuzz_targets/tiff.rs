//! Fuzz target: TIFF/DNG main-image data and Canon white levels (see hyperfocal::fuzzing::tiff).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| hyperfocal::fuzzing::tiff(data));
