//! Fuzz target: XMP packets (see hyperfocal::fuzzing::xmp).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| hyperfocal::fuzzing::xmp(data));
