//! Words typed into a shell.

/// `word` as a POSIX shell (and fish) reads it back as one word: as is when every character is
/// plain, else in single quotes, a quote inside closing, escaping and reopening them.
///
/// A leading `~` or `=` is not plain: the shell would expand it (zsh expands `=cmd` to the
/// command's path).
#[must_use]
pub fn shell_quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+,:@%=~".contains(c);
    let expands = word.starts_with(['~', '=']);
    if !word.is_empty() && !expands && word.chars().all(plain) {
        return word.to_owned();
    }
    let mut out = String::with_capacity(word.len().saturating_add(2));
    out.push('\'');
    for c in word.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_word_is_typed_as_is_and_anything_else_is_quoted_once() {
        assert_eq!(shell_quote("/tmp/a-b_c.txt"), "/tmp/a-b_c.txt");
        assert_eq!(shell_quote("--flag=1"), "--flag=1");
        assert_eq!(shell_quote("/tmp/a~b"), "/tmp/a~b");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("/tmp/it's"), r"'/tmp/it'\''s'");
        assert_eq!(shell_quote("/tmp/$HOME;rm"), "'/tmp/$HOME;rm'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn a_word_the_shell_would_expand_is_quoted() {
        assert_eq!(shell_quote("~/x"), "'~/x'");
        assert_eq!(shell_quote("=ls"), "'=ls'");
    }
}
