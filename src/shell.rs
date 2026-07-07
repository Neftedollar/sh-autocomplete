pub const BASH_COMPLETION: &str = include_str!("../shell/bash/shac.bash");
pub const ZSH_COMPLETION: &str = include_str!("../shell/zsh/shac.zsh");
pub const FISH_COMPLETION: &str = include_str!("../shell/fish/shac.fish");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    Posix,
}

impl Shell {
    pub fn parse(s: Option<&str>) -> Shell {
        match s.map(str::to_ascii_lowercase).as_deref() {
            Some("zsh") => Shell::Zsh,
            Some("bash") => Shell::Bash,
            Some("fish") => Shell::Fish,
            _ => Shell::Posix,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_known_and_unknown() {
        assert_eq!(Shell::parse(Some("zsh")), Shell::Zsh);
        assert_eq!(Shell::parse(Some("bash")), Shell::Bash);
        assert_eq!(Shell::parse(Some("fish")), Shell::Fish);
        assert_eq!(Shell::parse(Some("nu")), Shell::Posix);
        assert_eq!(Shell::parse(None), Shell::Posix);
    }
}
