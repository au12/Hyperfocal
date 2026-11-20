//! Fuzz target: GPS tracks (see hyperfocal::fuzzing::track).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| hyperfocal::fuzzing::track(data));
