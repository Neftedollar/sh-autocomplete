//! Reversible TSV field encoding for shell-tsv-v3. Order matters: backslash
//! first on encode, so decode is unambiguous.
pub fn encode_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

pub fn decode_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_control_and_backslash() {
        for raw in ["plain", "a\tb", "a\nb", "a\\b", "a\\tb", "\r", "→ ~/x"] {
            assert_eq!(decode_field(&encode_field(raw)), raw);
        }
        assert_eq!(encode_field("a\tb"), "a\\tb");
        assert_eq!(encode_field("a\\b"), "a\\\\b");
    }
}
