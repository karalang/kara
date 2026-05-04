#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        // Full pipeline must never panic — errors are fine
        let parsed = karac::parse(source);
        if parsed.errors.is_empty() {
            let resolved = karac::resolve(&parsed.program);
            if resolved.errors.is_empty() {
                let typed = karac::typecheck(&parsed.program, &resolved);
                let _ = karac::effectcheck(&parsed.program);
                let _ = karac::ownershipcheck(&parsed.program, &typed);
            }
        }
    }
});
