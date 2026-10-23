//! Rich-display bundles for cell results.
//!
//! Jupyter's `display_data` / `execute_result` messages carry a mime-
//! bundle map (`{ "text/plain": "...", "text/html": "...", ... }`).
//! [`DisplayBundle`] is that map. `%timeline` and `%rc` fill it today; the
//! `%show` renderer walked the legacy interpreter's values and went with it
//! at M3.

use std::collections::HashMap;

/// One cell result rendered into one or more mime types. The order of
/// `mimes` is preserved so callers (and tests) can rely on `text/plain`
/// always appearing first. Jupyter frontends pick the richest mime they
/// understand from the bundle; `text/plain` is the universal fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayBundle {
    pub mimes: Vec<(String, String)>,
}

impl DisplayBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, mime: impl Into<String>, body: impl Into<String>) -> Self {
        self.mimes.push((mime.into(), body.into()));
        self
    }

    pub fn get(&self, mime: &str) -> Option<&str> {
        self.mimes
            .iter()
            .find_map(|(m, b)| (m == mime).then_some(b.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.mimes.is_empty()
    }

    /// Lossy conversion to a `HashMap` — useful for plumbing into JSON
    /// serialization where the bundle becomes a JSON object.
    pub fn into_map(self) -> HashMap<String, String> {
        self.mimes.into_iter().collect()
    }
}
