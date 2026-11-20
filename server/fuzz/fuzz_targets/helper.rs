//! Fuzz target: the native helper's output (see hyperfocal::fuzzing::helper).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| hyperfocal::fuzzing::helper(data));
