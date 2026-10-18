// slopty: Slopty's own module (not in zeron).
//! Links to chats as live chips. A `slopty://open/chat/…` link in a reply (the link the
//! sidebar's Copy link puts on the clipboard) shows the chat as it stands now, its title and
//! whether it works or waits on the person, in a wash, rather than the words or address it was
//! written with; a click opens the chat as the link always did. Copy and selection keep what
//! was written.

use std::sync::Arc;

use super::parser::{Block, BlockTree, InlineRun, TopBlock};

/// Whether `url` links to a chat.
pub fn is_chat_link(url: &str) -> bool {
    crate::links::parse_slopty_conversation_link(url).is_ok()
}

/// What a chip says for a chat titled `title` (none: "Untitled chat") standing at `status`:
/// its title, then how it stands when it works or waits on the person.
pub fn chip_label(title: Option<&str>, status: Option<slopty_view_proto::SessionStatus>) -> String {
    use slopty_view_proto::SessionStatus;
    let title = title.map(str::trim).filter(|title| !title.is_empty()).unwrap_or("Untitled chat");
    let title = crate::transcript::single_line(title);
    match status {
        Some(SessionStatus::Working) => format!("{title} · Working"),
        Some(SessionStatus::AwaitingInput) => format!("{title} · Needs you"),
        Some(SessionStatus::Errored) => format!("{title} · Failed"),
        _ => title,
    }
}

/// `tree` with every link to a chat `label` knows shown as `label` says it, or `None` when it
/// links to none it knows (the caller keeps `tree`).
pub fn labelled(
    tree: &BlockTree,
    label: &dyn Fn(&str) -> Option<String>,
) -> Option<BlockTree> {
    let mut changed = false;
    let blocks = tree
        .blocks
        .iter()
        .map(|top| match block(&top.block, label) {
            Some(block) => {
                changed = true;
                Arc::new(TopBlock { range: top.range.clone(), block })
            }
            None => top.clone(),
        })
        .collect();
    changed.then_some(BlockTree { blocks })
}

fn block(block: &Block, label: &dyn Fn(&str) -> Option<String>) -> Option<Block> {
    match block {
        Block::Paragraph { runs } => self::runs(runs, label).map(|runs| Block::Paragraph { runs }),
        Block::Heading { level, runs } => {
            self::runs(runs, label).map(|runs| Block::Heading { level: *level, runs })
        }
        Block::BlockQuote { children } => {
            blocks(children, label).map(|children| Block::BlockQuote { children })
        }
        Block::List { ordered_start, items } => {
            let mut changed = false;
            let items = items
                .iter()
                .map(|item| match blocks(item, label) {
                    Some(item) => {
                        changed = true;
                        item
                    }
                    None => item.clone(),
                })
                .collect();
            changed.then(|| Block::List { ordered_start: *ordered_start, items })
        }
        Block::Table { .. } | Block::CodeBlock { .. } | Block::Rule | Block::Math { .. } => None,
    }
}

fn blocks(children: &[Block], label: &dyn Fn(&str) -> Option<String>) -> Option<Vec<Block>> {
    let mut changed = false;
    let children = children
        .iter()
        .map(|child| match block(child, label) {
            Some(child) => {
                changed = true;
                child
            }
            None => child.clone(),
        })
        .collect();
    changed.then_some(children)
}

/// The runs of one chat link become one run showing the chip's words, its text all the runs'
/// text, so the chip reads once however the link's words were styled and copy keeps them.
fn runs(runs: &[InlineRun], label: &dyn Fn(&str) -> Option<String>) -> Option<Vec<InlineRun>> {
    let mut changed = false;
    let mut out: Vec<InlineRun> = Vec::with_capacity(runs.len());
    let mut open: Option<String> = None;
    for run in runs {
        let url = run.style.link.as_deref();
        if let (Some(url), Some(last)) = (url, out.last_mut())
            && open.as_deref() == Some(url)
        {
            last.text.push_str(&run.text);
            continue;
        }
        open = None;
        let mut run = run.clone();
        if let Some(url) = url
            && is_chat_link(url)
            && let Some(words) = label(url)
        {
            run.style.file_label = Some(words);
            open = Some(url.to_owned());
            changed = true;
        }
        out.push(run);
    }
    changed.then_some(out)
}

/// `block` with each run's text the words it is drawn with: a relabelled link's label (a chat's
/// chip, a file by its name) in place of what was written.
pub fn as_drawn(block: &Block) -> Block {
    let runs = |runs: &[InlineRun]| -> Vec<InlineRun> {
        runs.iter()
            .map(|run| match &run.style.file_label {
                Some(label) => InlineRun { text: label.clone(), style: run.style.clone() },
                None => run.clone(),
            })
            .collect()
    };
    match block {
        Block::Paragraph { runs: r } => Block::Paragraph { runs: runs(r) },
        Block::Heading { level, runs: r } => Block::Heading { level: *level, runs: runs(r) },
        Block::BlockQuote { children } => {
            Block::BlockQuote { children: children.iter().map(as_drawn).collect() }
        }
        Block::List { ordered_start, items } => Block::List {
            ordered_start: *ordered_start,
            items: items.iter().map(|item| item.iter().map(as_drawn).collect()).collect(),
        },
        Block::Table { .. } | Block::CodeBlock { .. } | Block::Rule | Block::Math { .. } => {
            block.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parse_full;

    fn link(chat: &str) -> String {
        crate::links::slopty_conversation_link(chat, "here")
    }

    #[test]
    fn a_link_to_a_chat_shows_it_as_it_stands() {
        let text = format!("See [the **other** chat]({}) and [a page](https://example.com).", link("b"));
        let tree = parse_full(&text);
        let label = |url: &str| {
            let chat = crate::links::parse_slopty_conversation_link(url).ok()?.chat_id;
            (chat == "b").then(|| {
                chip_label(Some("Fix the parser"), Some(slopty_view_proto::SessionStatus::Working))
            })
        };
        let chipped = labelled(&tree, &label).expect("the chat link is relabelled");
        let Block::Paragraph { runs } = &chipped.blocks[0].block else { panic!("a paragraph") };
        let shown: String = runs
            .iter()
            .map(|run| run.style.file_label.clone().unwrap_or_else(|| run.text.clone()))
            .collect();
        assert_eq!(shown, "See Fix the parser · Working and a page.");
        let written: String = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(written, "See the other chat and a page.", "copy keeps what was written");
        assert!(labelled(&parse_full("[a page](https://example.com)"), &label).is_none());
        let unknown = format!("[gone]({})", link("z"));
        assert!(labelled(&parse_full(&unknown), &label).is_none(), "a chat it does not know");
    }

    #[test]
    fn a_chip_says_the_title_and_how_the_chat_stands() {
        use slopty_view_proto::SessionStatus;
        assert_eq!(chip_label(None, None), "Untitled chat");
        assert_eq!(chip_label(Some("  "), Some(SessionStatus::Done)), "Untitled chat");
        assert_eq!(
            chip_label(Some("Ship it"), Some(SessionStatus::AwaitingInput)),
            "Ship it · Needs you"
        );
    }
}
