//! Identifier case conversion shared by both interpreters and both
//! codegens (an enum variant's `Display` text).

/// Convert a PascalCase identifier to lower_snake_case.
/// `InProgress` → `in_progress`, `Up` → `up`, `HTTPError` → `h_t_t_p_error`.
pub fn pascal_to_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.extend(ch.to_lowercase());
    }
    result
}
