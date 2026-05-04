#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        // Parser must never panic on any input — errors are fine, panics are not
        let _ = karac::parse(source);
    }
});
