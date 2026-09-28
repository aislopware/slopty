//! The composer's menus, as text: which one the caret is in, what it lists, and what picking
//! a row writes into the draft.
//!
//! - **Slash commands.** A draft that starts with `/`, the caret still in its first word, is asking
//!   for a command: the menu lists the commands whose name or description holds what follows the
//!   slash. Picking one writes `/name ` over that word. Only there: a `/` anywhere else in a draft
//!   is text, and so is a draft of more than one line, which the composer pastes as a message
//!   ([`super::composer::is_command`]).
//! - **Mentions.** An `@` at the start of a word, the caret still in that word, is asking for a
//!   file: the worker lists the paths under the agent's directory the rest of the word matches.
//!   Picking one writes `@path ` over the word; a folder writes `@folder/` and keeps asking, one
//!   level down. Claude Code reads `@path` itself, so the draft stays plain text.
//!
//! Offsets are UTF-8 byte offsets into the draft, as the composer's caret is.

use slopty_proto::conversation::{CommandSource, SlashCommand};

/// Rows a menu shows at most; the rest are reached by typing more.
pub const ROWS: usize = 50;

/// Paths one `@` query asks the worker for.
pub const MENTIONS: u32 = 50;

/// The word the caret is in, when it asks for a menu.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Token {
    /// `/…` at the start of a one-line draft: what follows the slash.
    Command {
        /// What was typed after the slash.
        query: String,
    },
    /// `@…` at the start of a word: where the `@` is, and what follows it.
    Mention {
        /// The byte offset of the `@`.
        at: usize,
        /// What was typed after the `@`, up to the caret.
        query: String,
    },
}

impl Token {
    /// Where the word starts: the menu stays closed for it once Esc closed it there.
    #[must_use]
    pub const fn start(&self) -> usize {
        match self {
            Self::Command { .. } => 0,
            Self::Mention { at, .. } => *at,
        }
    }
}

/// The menu `text` asks for with the caret at `caret`, if any.
#[must_use]
pub fn token(text: &str, caret: usize) -> Option<Token> {
    let before = text.get(..caret)?;
    if let Some(query) = before.strip_prefix('/')
        && !query.contains(char::is_whitespace)
        && !text.contains('\n')
    {
        return Some(Token::Command { query: query.to_owned() });
    }
    let word_start = before.rfind(char::is_whitespace).map_or(0, |ix| ix.saturating_add(1));
    let word = before.get(word_start..)?;
    let query = word.strip_prefix('@')?;
    Some(Token::Mention { at: word_start, query: query.to_owned() })
}

/// The commands `query` matches, best first, at most [`ROWS`].
///
/// A name that starts with it comes first, then a name that holds it, then a description that
/// holds every word of it; within each, the project's and the person's before plugins' and
/// Claude Code's own, then by name.
#[must_use]
pub fn commands<'a>(all: &'a [SlashCommand], query: &str) -> Vec<&'a SlashCommand> {
    let needle = query.to_lowercase();
    let source_rank = |source: CommandSource| match source {
        CommandSource::Project => 0_u8,
        CommandSource::Personal => 1,
        CommandSource::Plugin => 2,
        CommandSource::BuiltIn => 3,
    };
    let mut ranked: Vec<(u8, u8, &SlashCommand)> = all
        .iter()
        .filter_map(|command| {
            let name = command.name.to_lowercase();
            let rank = if name.starts_with(&needle) {
                0
            } else if name.contains(&needle) {
                1
            } else if crate::picker::matches(query, &command.description) {
                2
            } else {
                return None;
            };
            Some((rank, source_rank(command.source), command))
        })
        .collect();
    ranked.sort_by(|a, b| (a.0, a.1, &a.2.name).cmp(&(b.0, b.1, &b.2.name)));
    ranked.into_iter().take(ROWS).map(|(_, _, command)| command).collect()
}

/// Where a command comes from, as the menu's right edge says it; nothing for Claude Code's
/// own, which are most of the list.
#[must_use]
pub const fn source_label(source: CommandSource) -> Option<&'static str> {
    match source {
        CommandSource::BuiltIn => None,
        CommandSource::Personal => Some("Personal"),
        CommandSource::Project => Some("Project"),
        CommandSource::Plugin => Some("Plugin"),
    }
}

/// The draft once command `name` is picked, and where the caret goes: `/name ` in place of the
/// first word, the rest of the draft kept.
#[must_use]
pub fn pick_command(text: &str, name: &str) -> (String, usize) {
    let rest = text.find(char::is_whitespace).map_or("", |ix| text.get(ix..).unwrap_or_default());
    let head = format!("/{name} ");
    let caret = head.len();
    (format!("{head}{}", rest.trim_start()), caret)
}

/// The draft once `path` is picked for the mention at `at`, and where the caret goes.
///
/// The caret was at `caret`. A file ends its word with a space; a folder (`…/`) leaves the
/// caret after its slash, so the menu goes on one level down. A path with a space is quoted,
/// as Claude Code reads it.
#[must_use]
pub fn pick_path(text: &str, at: usize, caret: usize, path: &str) -> (String, usize) {
    let head = text.get(..at).unwrap_or(text);
    let rest = text.get(caret..).unwrap_or_default();
    let rest = rest.find(char::is_whitespace).map_or("", |ix| rest.get(ix..).unwrap_or_default());
    let folder = path.ends_with('/');
    let written = if path.contains(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    };
    let space = if folder || rest.starts_with(char::is_whitespace) { "" } else { " " };
    let before = format!("{head}{written}{space}");
    let caret = if folder { head.len().saturating_add(written.len()) } else { before.len() };
    (format!("{before}{rest}"), caret)
}

/// The `@path` mentions in a sent prompt's text, as byte ranges over the `@` and the path; a
/// quoted path's range covers its quotes.
#[must_use]
pub fn mentions(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = text.get(from..).and_then(|rest| rest.find('@')) {
        let at = from.saturating_add(found);
        let starts_word =
            text.get(..at).and_then(|b| b.chars().next_back()).is_none_or(char::is_whitespace);
        let after = text.get(at.saturating_add(1)..).unwrap_or_default();
        let len = if let Some(quoted) = after.strip_prefix('"') {
            quoted.find('"').map_or(0, |end| end.saturating_add(2))
        } else {
            after.find(|c: char| c.is_whitespace()).unwrap_or(after.len())
        };
        // A trailing full stop or comma ends the sentence, not the path.
        let word = after.get(..len).unwrap_or_default();
        let len = len.saturating_sub(
            word.len().saturating_sub(word.trim_end_matches([',', '.', ';', ':', ')']).len()),
        );
        let end = at.saturating_add(1).saturating_add(len);
        if starts_word && len > 0 {
            out.push(at..end);
        }
        from = end.max(at.saturating_add(1));
    }
    out
}

/// The path a mention names: its text without the `@` and any quotes.
#[must_use]
pub fn mention_path(mention: &str) -> &str {
    let path = mention.strip_prefix('@').unwrap_or(mention);
    path.strip_prefix('"').and_then(|p| p.strip_suffix('"')).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, description: &str, source: CommandSource) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: description.to_owned(),
            argument_hint: None,
            source,
        }
    }

    /// A menu opens for a draft's leading `/` and for an `@` that starts a word, while the
    /// caret is in that word; never for a `/` mid-line or a draft of two lines.
    #[test]
    fn a_menu_opens_only_for_a_leading_slash_or_a_words_at() {
        let command = |q: &str| Some(Token::Command { query: q.to_owned() });
        assert_eq!(token("/", 1), command(""));
        assert_eq!(token("/comp", 5), command("comp"));
        assert_eq!(token("/compact now", 12), None, "past the command's word");
        assert_eq!(token("/compact now", 3), command("co"));
        assert_eq!(token("fix a/b", 7), None, "a slash mid-line");
        assert_eq!(token("see /tmp", 8), None);
        assert_eq!(token("/multi\nline", 3), None, "a message, not a command");
        assert_eq!(
            token("look at @src/ma", 15),
            Some(Token::Mention { at: 8, query: "src/ma".to_owned() })
        );
        assert_eq!(token("@", 1), Some(Token::Mention { at: 0, query: String::new() }));
        assert_eq!(token("mail me@host", 12), None, "an @ inside a word");
        assert_eq!(token("@a.rs then", 10), None, "the caret left the word");
    }

    /// Commands rank by name before description, the project's before Claude Code's own.
    #[test]
    fn commands_rank_by_name_then_description() {
        let all = [
            command("compact", "Free up context", CommandSource::BuiltIn),
            command("commit", "Commit the staged work", CommandSource::Project),
            command("review", "Review the diff before a commit", CommandSource::Personal),
            command("model", "Set the AI model", CommandSource::BuiltIn),
        ];
        let names = |q: &str| commands(&all, q).iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("com"), ["commit", "compact", "review"], "the project's first");
        assert_eq!(names("mit"), ["commit", "review"], "a name before a description");
        assert_eq!(names(""), ["commit", "review", "compact", "model"]);
        assert!(names("zzz").is_empty());
    }

    /// Picking writes the command or the path over the word, keeps the rest, and puts the caret
    /// where typing goes on; a folder keeps the mention open one level down.
    #[test]
    fn picking_writes_over_the_word() {
        assert_eq!(pick_command("/comp", "compact"), ("/compact ".to_owned(), 9));
        assert_eq!(pick_command("/co keep it", "compact"), ("/compact keep it".to_owned(), 9));
        assert_eq!(pick_path("see @ma", 4, 7, "src/main.rs"), ("see @src/main.rs ".to_owned(), 17));
        assert_eq!(pick_path("see @s and", 4, 6, "src/"), ("see @src/ and".to_owned(), 9));
        assert_eq!(pick_path("@sh", 0, 3, "a b.png"), ("@\"a b.png\" ".to_owned(), 11));
    }

    /// A sent prompt's mentions are the words that start with `@`, a sentence's full stop left
    /// off, a quoted path whole.
    #[test]
    fn a_prompt_names_its_mentions() {
        let text = "Read @src/main.rs and @\"a b.png\". Mail me@host. @docs/";
        let found: Vec<&str> = mentions(text).into_iter().filter_map(|r| text.get(r)).collect();
        assert_eq!(found, ["@src/main.rs", "@\"a b.png\"", "@docs/"]);
        assert_eq!(mention_path("@\"a b.png\""), "a b.png");
        assert_eq!(mention_path("@src/main.rs"), "src/main.rs");
        assert!(mentions("@ alone").is_empty());
    }
}
