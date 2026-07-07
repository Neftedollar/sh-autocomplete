use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenRole {
    Command,
    SubcommandOrArg,
    Option,
    Path,
}

#[derive(Debug, Clone)]
pub struct ParsedContext {
    pub line_before_cursor: String,
    pub tokens: Vec<String>,
    pub active_token: String,
    pub active_index: usize,
    pub role: TokenRole,
    pub command: Option<String>,
    pub prev_token: Option<String>,
    pub project_markers: Vec<String>,
    /// The unterminated quote char (`"` or `'`) of the active token, if any.
    pub open_quote: Option<char>,
}

pub fn parse(line: &str, cursor: usize, cwd: &Path) -> ParsedContext {
    // `cursor` is a CHARACTER offset (matching zsh $CURSOR / fish), not a byte
    // offset, so multibyte text before the cursor must not be byte-sliced.
    let before: String = line.chars().take(cursor).collect();
    // Only the pipeline/list segment the cursor is in determines the command:
    // completion after `|`, `&&`, `;`, `&`, or a redirection targets the
    // command that starts that segment, not the one at the start of the line.
    let segment = last_segment(&before);
    let scanned = tokenize(&segment);
    let mut tokens = scanned.tokens;

    if scanned.trailing_boundary {
        tokens.push(String::new());
    }

    let active_index = tokens.len().saturating_sub(1);
    let active_token = tokens.get(active_index).cloned().unwrap_or_default();
    let command = tokens
        .iter()
        .find(|token| !token.is_empty() && !token.starts_with('-'))
        .cloned();
    let prev_token = if active_index > 0 {
        tokens.get(active_index - 1).cloned()
    } else {
        None
    };
    let role = classify_role(&tokens, active_index, cwd);
    let project_markers = detect_project_markers(cwd);

    ParsedContext {
        line_before_cursor: before,
        tokens,
        active_token,
        active_index,
        role,
        command,
        prev_token,
        project_markers,
        open_quote: scanned.open_quote,
    }
}

/// Return the char index in `s` immediately after the last unquoted
/// pipeline/list operator (`|`, `||`, `&&`, `;`, `&`) or redirection (`<`,
/// `>`), so callers can restrict tokenization to the segment containing the
/// cursor. Operators inside an open quote are not boundaries. 0 if none.
fn last_segment_start(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut boundary = 0usize;
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
            }
            '|' | '&' | ';' | '<' | '>' if quote.is_none() => {
                // Treat a doubled operator ("||", "&&") as a single boundary.
                if matches!(ch, '|' | '&') && chars.get(i + 1) == Some(&ch) {
                    i += 1;
                }
                boundary = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    boundary
}

/// The trailing pipeline/list segment of `s` (the part after the last
/// top-level operator), or the whole string if there is none.
fn last_segment(s: &str) -> String {
    let start = last_segment_start(s);
    s.chars().skip(start).collect()
}

fn classify_role(tokens: &[String], active_index: usize, cwd: &Path) -> TokenRole {
    let token = tokens
        .get(active_index)
        .map(String::as_str)
        .unwrap_or_default();
    if active_index == 0 {
        return TokenRole::Command;
    }
    if token.starts_with('-') {
        return TokenRole::Option;
    }
    if looks_like_path(token, cwd) {
        return TokenRole::Path;
    }
    TokenRole::SubcommandOrArg
}

fn looks_like_path(token: &str, cwd: &Path) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token.starts_with("~/")
        || token.contains('/')
    {
        return true;
    }
    cwd.join(token).exists()
}

fn detect_project_markers(cwd: &Path) -> Vec<String> {
    let mut markers = Vec::new();
    for name in [
        ".git",
        "package.json",
        "pnpm-lock.yaml",
        "Cargo.toml",
        "*.csproj",
        "*.sln",
        "pyproject.toml",
        "Dockerfile",
        "Makefile",
    ] {
        if find_upwards(cwd, name).is_some() {
            markers.push(name.to_string());
        }
    }
    markers
}

fn find_upwards(cwd: &Path, name: &str) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(path) = current {
        if let Some(pattern) = name.strip_prefix("*.") {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let candidate = entry.path();
                    if candidate
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext == pattern)
                    {
                        return Some(candidate);
                    }
                }
            }
        } else {
            let candidate = path.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        current = path.parent();
    }
    None
}

struct Tokenized {
    tokens: Vec<String>,
    /// The unterminated quote char at end-of-scan, if the input ends inside
    /// an open quote.
    open_quote: Option<char>,
    /// True if the scan's LAST character was an unescaped, unquoted
    /// whitespace: the user finished a word and a fresh token starts empty.
    /// False if the trailing whitespace was escaped or inside a quote, in
    /// which case it is content, not a boundary (kept in the active token).
    trailing_boundary: bool,
}

fn tokenize(line: &str) -> Tokenized {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut trailing_boundary = false;

    for ch in line.chars() {
        trailing_boundary = false;
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                } else {
                    current.push(ch);
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                trailing_boundary = true;
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    Tokenized {
        tokens,
        open_quote: quote,
        trailing_boundary,
    }
}

pub fn shell_split(line: &str) -> Vec<String> {
    tokenize(line).tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_basic_shell_line() {
        assert_eq!(
            shell_split("git checkout feat"),
            vec!["git", "checkout", "feat"]
        );
    }

    #[test]
    fn preserves_quoted_segments() {
        assert_eq!(
            shell_split("echo \"hello world\""),
            vec!["echo", "hello world"]
        );
    }

    #[test]
    fn cursor_is_character_offset() {
        // "привет " has 7 chars; cursor 7 = end. Byte length is 13.
        let cwd = std::path::Path::new("/tmp");
        let ctx = parse("привет ls", 8, cwd); // 8 chars = after "привет l"
        assert_eq!(ctx.active_token, "l");
    }

    #[test]
    fn command_after_operator() {
        let cwd = std::path::Path::new("/tmp");
        let ctx = parse("git log | grep fo", 17, cwd);
        assert_eq!(ctx.command.as_deref(), Some("grep"));
        assert_eq!(ctx.active_token, "fo");
        let ctx2 = parse("a && b -x", 9, cwd);
        assert_eq!(ctx2.command.as_deref(), Some("b"));
    }

    #[test]
    fn escaped_trailing_space_keeps_token() {
        let cwd = std::path::Path::new("/tmp");
        let ctx = parse("cd My\\ ", 7, cwd);
        assert_eq!(ctx.active_token, "My ");
        assert_eq!(ctx.open_quote, None);
        let q = parse("cd \"My ", 7, cwd);
        assert_eq!(q.active_token, "My ");
        assert_eq!(q.open_quote, Some('"'));
    }
}
