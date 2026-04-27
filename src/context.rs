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
}

pub fn parse(line: &str, cursor: usize, cwd: &Path) -> ParsedContext {
    // Round down to the nearest UTF-8 char boundary so multibyte chars (e.g.
    // Cyrillic, CJK) don't cause a panic when the cursor lands mid-codepoint.
    let max = cursor.min(line.len());
    let safe_cursor = (0..=max).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
    let before = line[..safe_cursor].to_string();
    let mut tokens = shell_split(&before);
    let ends_with_space = before.ends_with(char::is_whitespace);

    if ends_with_space {
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
    }
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

pub fn shell_split(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in line.chars() {
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
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
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
}
