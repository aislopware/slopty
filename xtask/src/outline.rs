//! `xtask outline`: a Rust file's items, one line each with its line range, nested under their
//! `mod`, `impl`, `trait`, `extern` block or `fn`, so a reader takes the part it needs instead of
//! the whole file (`docs/decisions/tooling.md`, "A round's long steps are the model's, not the
//! checks'").
//!
//! It scans tokens rather than parsing: strings, raw strings, chars, lifetimes and comments are
//! taken whole, delimiters are matched, and an item is known by its head where a statement
//! starts. So it outlines a file mid-edit that a parser would refuse, and adds no dependency.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use clap::Args;

/// `xtask outline` options.
#[derive(Args, Debug)]
pub struct OutlineOpts {
    /// The Rust files to outline.
    #[arg(required = true)]
    files: Vec<Utf8PathBuf>,
}

/// Print each file's outline: a header with its length, then one line per item, its range
/// first, two spaces deeper per level of nesting.
///
/// # Errors
///
/// When a file cannot be read.
pub fn run(opts: &OutlineOpts) -> Result<()> {
    let mut out = String::new();
    for (n, file) in opts.files.iter().enumerate() {
        let text = std::fs::read_to_string(file).with_context(|| format!("read {file}"))?;
        if n > 0 {
            out.push('\n');
        }
        let _infallible = writeln!(out, "{file}: {} lines", text.lines().count());
        write_items(&mut out, &outline(&text), 0);
    }
    print!("{out}");
    Ok(())
}

/// One item and the items inside it.
#[derive(Debug, PartialEq, Eq)]
pub struct Item {
    /// `fn`, `struct`, `enum`, `union`, `trait`, `impl`, `mod`, `const`, `static`, `type`,
    /// `extern` or `macro_rules!`; none for a macro's invocation that holds items, whose name
    /// is its path (`proptest!`).
    pub kind: &'static str,
    /// Its name; an `impl`'s is its head (`Display for Version`), an `extern` block's its ABI.
    pub name: String,
    /// Its visibility as written (`pub`, `pub(crate)`), when it has one.
    pub vis: Option<String>,
    /// The line of its head (after its attributes and doc comment), from 1.
    pub start: usize,
    /// The line of its closing brace, or of the `;` that ends it.
    pub end: usize,
    /// The items it holds: a module's, an impl's or a trait's, an `extern` block's, and those
    /// declared in a function's body.
    pub items: Vec<Self>,
}

fn write_items(out: &mut String, items: &[Item], depth: usize) {
    for item in items {
        let indent = depth.saturating_mul(2);
        let _infallible = write!(out, "{:indent$}{}", "", item.start);
        if item.end != item.start {
            let _infallible = write!(out, "-{}", item.end);
        }
        if let Some(vis) = &item.vis {
            let _infallible = write!(out, " {vis}");
        }
        let kind = if item.kind.is_empty() { String::new() } else { format!(" {}", item.kind) };
        let _infallible = writeln!(out, "{kind} {}", item.name);
        write_items(out, &item.items, depth.saturating_add(1));
    }
}

/// The items of the Rust source `src`.
pub fn outline(src: &str) -> Vec<Item> {
    let tokens = lex(src);
    let mut parser = Parser { src, end: tokens.len(), tokens, pos: 0 };
    parser.items()
}

/// `i` and `n` tokens or bytes on; positions are bounded by the source's length, far from
/// overflow.
const fn on(i: usize, n: usize) -> usize {
    i.saturating_add(n)
}

/// What a token is, as far as the outline needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A word: an identifier or a keyword (`r#type` included, as itself).
    Word,
    /// A string, char or number: never a delimiter or a keyword, whatever it holds.
    Literal,
    /// `'a`, `'static`, a loop label.
    Lifetime,
    /// One punctuation character, or `->`, `=>` or `::`.
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Kind,
    /// Its bytes in the source.
    start: usize,
    end: usize,
    /// Its first line, from 1.
    line: usize,
}

/// The tokens of `src`, comments dropped.
fn lex(src: &str) -> Vec<Token> {
    let mut lexer = Lexer { src, bytes: src.as_bytes(), at: 0, line: 1 };
    // A shebang line is no token; an inner attribute (`#![…]`) is.
    if src.starts_with("#!") && !src.starts_with("#![") {
        lexer.at = src.find('\n').unwrap_or(src.len());
    }
    std::iter::from_fn(|| lexer.token()).collect()
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    at: usize,
    line: usize,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(on(self.at, ahead)).copied()
    }

    const fn bump(&mut self, n: usize) {
        self.at = on(self.at, n);
    }

    /// Past one byte, counting it when it ends a line.
    fn bump_counted(&mut self, b: u8) {
        self.line = on(self.line, usize::from(b == b'\n'));
        self.bump(1);
    }

    fn rest(&self) -> &[u8] {
        self.bytes.get(self.at..).unwrap_or_default()
    }

    /// Past every byte `keep` holds to.
    fn bump_while(&mut self, keep: impl Fn(u8) -> bool) {
        while let Some(b) = self.peek(0).filter(|&b| keep(b)) {
            self.bump_counted(b);
        }
    }

    /// The next token, past whitespace and comments.
    fn token(&mut self) -> Option<Token> {
        loop {
            match (self.peek(0)?, self.peek(1)) {
                (b'/', Some(b'/')) => self.bump_while(|b| b != b'\n'),
                (b'/', Some(b'*')) => self.block_comment(),
                (b, _) if b.is_ascii_whitespace() => self.bump_counted(b),
                _ => break,
            }
        }
        let (start, line) = (self.at, self.line);
        let kind = match self.peek(0)? {
            b'"' => {
                self.bump(1);
                self.string();
                Kind::Literal
            }
            b'\'' => self.quote(),
            b'0'..=b'9' => {
                self.bump_while(continues_word);
                Kind::Literal
            }
            b if starts_word(b) => self.word(),
            _ => {
                let two = self.rest().get(..2);
                let joined = matches!(two, Some(b"->" | b"=>" | b"::"));
                self.bump(if joined { 2 } else { 1 });
                Kind::Punct
            }
        };
        Some(Token { kind, start, end: self.at, line })
    }

    /// Past a block comment, the ones nested in it included.
    fn block_comment(&mut self) {
        self.bump(2);
        let mut depth = 1_usize;
        while depth > 0 {
            match (self.peek(0), self.peek(1)) {
                (None, _) => return,
                (Some(b'/'), Some(b'*')) => {
                    depth = on(depth, 1);
                    self.bump(2);
                }
                (Some(b'*'), Some(b'/')) => {
                    depth = depth.saturating_sub(1);
                    self.bump(2);
                }
                (Some(b), _) => self.bump_counted(b),
            }
        }
    }

    /// Past a string's closing quote, from just inside its opening one.
    fn string(&mut self) {
        while let Some(b) = self.peek(0) {
            self.bump(1);
            match b {
                b'"' => return,
                // The escaped byte, which may end the line.
                b'\\' => {
                    if let Some(escaped) = self.peek(0) {
                        self.bump_counted(escaped);
                    }
                }
                b'\n' => self.line = on(self.line, 1),
                _ => {}
            }
        }
    }

    /// Past a raw string, from its first `#` or its quote: a quote and as many `#` close it.
    fn raw_string(&mut self) {
        let hashes = self.rest().iter().take_while(|&&b| b == b'#').count();
        self.bump(on(hashes, 1));
        while let Some(b) = self.peek(0) {
            self.bump_counted(b);
            let closes = self.rest().iter().take(hashes).filter(|&&b| b == b'#').count() == hashes;
            if b == b'"' && closes {
                self.bump(hashes);
                return;
            }
        }
    }

    /// A char literal or a lifetime, from its quote: `'a'` and `'\n'` are chars, `'a` is not.
    fn quote(&mut self) -> Kind {
        self.bump(1);
        let rest = self.src.get(self.at..).unwrap_or_default();
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            // `'\n'`, `'\''`, `'\u{1F600}'`: the escaped character, then to the quote.
            (Some('\\'), _) => {
                self.bump(2);
                self.bump_while(|b| b != b'\'' && b != b'\n');
                if self.peek(0) == Some(b'\'') {
                    self.bump(1);
                }
                Kind::Literal
            }
            (Some(c), Some('\'')) => {
                self.bump(on(c.len_utf8(), 1));
                Kind::Literal
            }
            (Some(c), _) if c.is_alphabetic() || c == '_' => {
                self.bump_while(continues_word);
                Kind::Lifetime
            }
            _ => Kind::Punct,
        }
    }

    /// A word, or the literal it prefixes: `b"…"`, `c"…"`, `b'…'`, `r"…"`, `br#"…"#`.
    fn word(&mut self) -> Kind {
        let start = self.at;
        self.bump_while(continues_word);
        let prefix = self.src.get(start..self.at).unwrap_or_default();
        match (prefix, self.peek(0)) {
            ("b" | "c", Some(b'"')) => {
                self.bump(1);
                self.string();
                Kind::Literal
            }
            ("b", Some(b'\'')) => self.quote(),
            ("r" | "br" | "cr", Some(b'"' | b'#')) => {
                let hashes = self.rest().iter().take_while(|&&b| b == b'#').count();
                if self.peek(hashes) == Some(b'"') {
                    self.raw_string();
                } else if hashes == 1 && self.peek(1).is_some_and(starts_word) {
                    // A raw identifier, `r#type`: a word, never the keyword.
                    self.bump(1);
                    self.bump_while(continues_word);
                    return Kind::Word;
                } else {
                    return Kind::Word;
                }
                Kind::Literal
            }
            _ => Kind::Word,
        }
    }
}

/// Whether `b` starts a word: a letter, `_`, or any byte of a character beyond ASCII.
const fn starts_word(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

const fn continues_word(b: u8) -> bool {
    starts_word(b) || b.is_ascii_digit()
}

/// Where a head ends.
enum Stop {
    /// At this `{`, which opens the item's body.
    Body(usize),
    /// At this `;`, or at the last token before the enclosing block closes or the source ends.
    Last(usize),
}

/// An item's head, as read so far.
struct Head {
    kind: &'static str,
    name: String,
    vis: Option<String>,
    /// Its first token.
    first: usize,
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    /// The next token.
    pos: usize,
    /// One past the last token of the block being read.
    end: usize,
}

impl Parser<'_> {
    fn token(&self, i: usize) -> Option<&Token> {
        self.tokens.get(i).filter(|_| i < self.end)
    }

    fn text(&self, i: usize) -> &str {
        self.token(i).and_then(|t| self.src.get(t.start..t.end)).unwrap_or_default()
    }

    fn punct(&self, i: usize, text: &str) -> bool {
        self.token(i).is_some_and(|t| t.kind == Kind::Punct) && self.text(i) == text
    }

    fn word(&self, i: usize) -> Option<&str> {
        self.token(i).filter(|t| t.kind == Kind::Word).map(|_| self.text(i))
    }

    fn line(&self, i: usize) -> usize {
        self.tokens.get(i).map_or(1, |t| t.line)
    }

    fn opens(&self, i: usize) -> bool {
        self.punct(i, "{") || self.punct(i, "(") || self.punct(i, "[")
    }

    /// The block's last token.
    const fn last(&self) -> usize {
        self.end.saturating_sub(1)
    }

    /// The token that closes the delimiter at `open`, or the block's last one when none does.
    fn close(&self, open: usize) -> usize {
        let mut depth = 0_usize;
        for i in open..self.end {
            if self.token(i).is_some_and(|t| t.kind == Kind::Punct) {
                match self.text(i) {
                    "{" | "(" | "[" => depth = on(depth, 1),
                    "}" | ")" | "]" => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return i;
                        }
                    }
                    _ => {}
                }
            }
        }
        self.last()
    }

    /// The source from token `from` to token `to`, both included, on one line.
    fn spoken(&self, from: usize, to: usize) -> String {
        let span = self.token(from).zip(self.token(to)).map(|(a, b)| (a.start, b.end));
        let text = span.and_then(|(start, end)| self.src.get(start..end)).unwrap_or_default();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The items from here to the block's end. An item starts a statement (the block's first,
    /// or one after a `;` or a block), so the `const` of `*const u8` in a `let` is none.
    fn items(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        let mut fresh = true;
        while self.pos < self.end {
            let i = self.pos;
            if self.punct(i, "#") {
                // An attribute, outer or inner.
                let open = if self.punct(on(i, 1), "!") { on(i, 2) } else { on(i, 1) };
                self.pos = if self.punct(open, "[") { on(self.close(open), 1) } else { on(i, 1) };
                continue;
            }
            if fresh {
                if let Some(item) = self.item() {
                    items.push(item);
                    continue;
                }
                if self.pos != i {
                    // Past a `use`, an `extern crate` or a macro's invocation.
                    continue;
                }
            }
            // No item starts here: past this token, and into a block it opens, whose items (in
            // an `if`, a `match` arm, a closure, a bare block) belong to the item around it.
            if self.punct(i, "{") {
                let close = self.close(i);
                items.extend(self.inside(i, close));
                self.pos = on(close, 1);
                fresh = true;
            } else if self.opens(i) {
                self.pos = on(self.close(i), 1);
                fresh = false;
            } else {
                self.pos = on(i, 1);
                fresh = self.punct(i, ";");
            }
        }
        items
    }

    /// The items between the delimiters at `open` and `close`.
    fn inside(&mut self, open: usize, close: usize) -> Vec<Item> {
        let (pos, end) = (self.pos, self.end);
        (self.pos, self.end) = (on(open, 1), close);
        let items = self.items();
        (self.pos, self.end) = (pos, end);
        items
    }

    /// The item whose head starts here, past it; `None`, past what it skipped (a `use`, an
    /// `extern crate`, a macro's invocation) or not moved at all, when there is none.
    fn item(&mut self) -> Option<Item> {
        let first = self.pos;
        let mut i = first;
        let vis = (self.word(i) == Some("pub")).then(|| {
            let last = if self.punct(on(i, 1), "(") { self.close(on(i, 1)) } else { i };
            i = on(last, 1);
            self.spoken(first, last)
        });
        let head = |kind, name| Head { kind, name, vis: vis.clone(), first };
        // The qualifiers before the keyword.
        loop {
            let next = self.word(on(i, 1));
            match self.word(i) {
                Some("default" | "async" | "unsafe" | "safe" | "auto") if next.is_some() => {}
                Some("const") if matches!(next, Some("fn" | "unsafe" | "async" | "extern")) => {}
                Some("extern") if next == Some("crate") => {
                    self.pos = on(self.semicolon(i), 1);
                    return None;
                }
                Some("extern") => {
                    let abi = self.token(on(i, 1)).is_some_and(|t| t.kind == Kind::Literal);
                    let after = on(i, on(1, usize::from(abi)));
                    if self.punct(after, "{") {
                        let name = if abi { self.text(on(i, 1)).to_owned() } else { String::new() };
                        return Some(self.block(head("extern", name), after));
                    }
                    i = after;
                    continue;
                }
                _ => break,
            }
            i = on(i, 1);
        }
        let keyword = self.word(i)?;
        let named = self.word(on(i, 1)).map(str::to_owned);
        match keyword {
            "fn" | "struct" | "enum" | "union" | "trait" | "mod" => {
                let kind = keyword_kind(keyword);
                let head = head(kind, named?);
                Some(match self.head_end(on(i, 2)) {
                    Stop::Body(open) if matches!(kind, "fn" | "trait" | "mod") => {
                        self.block(head, open)
                    }
                    Stop::Body(open) => self.leaf(head, self.close(open)),
                    Stop::Last(last) => self.leaf(head, last),
                })
            }
            "impl" => {
                // Its head names it, past the impl's own generics: `Display for Version`.
                let mut from = on(i, 1);
                if self.punct(from, "<") {
                    from = on(self.angle_close(from), 1);
                }
                let Stop::Body(open) = self.head_end(from) else {
                    self.pos = on(self.semicolon(i), 1);
                    return None;
                };
                let until = (from..open).find(|&t| self.word(t) == Some("where")).unwrap_or(open);
                let name = if until > from {
                    self.spoken(from, until.saturating_sub(1))
                } else {
                    String::new()
                };
                Some(self.block(head("impl", name), open))
            }
            "const" | "static" => {
                let at = on(i, on(1, usize::from(self.word(on(i, 1)) == Some("mut"))));
                let head = head(keyword_kind(keyword), self.word(at)?.to_owned());
                let last = self.semicolon(at);
                Some(self.leaf(head, last))
            }
            "type" => {
                let head = head("type", named?);
                let last = self.semicolon(on(i, 2));
                Some(self.leaf(head, last))
            }
            "use" => {
                self.pos = on(self.semicolon(i), 1);
                None
            }
            "macro_rules" if self.punct(on(i, 1), "!") && self.opens(on(i, 3)) => {
                let head = head("macro_rules!", self.word(on(i, 2))?.to_owned());
                let close = self.close(on(i, 3));
                let last = if self.punct(on(close, 1), ";") { on(close, 1) } else { close };
                Some(self.leaf(head, last))
            }
            _ => {
                // A macro's invocation in an item's place (`proptest! { … }`,
                // `objc2::define_class!(…);`): an item when what it holds reads as items.
                let mut at = i;
                while self.word(at).is_some() || self.punct(at, "::") {
                    at = on(at, 1);
                }
                let open = on(at, 1);
                if at == i || !self.punct(at, "!") || !self.opens(open) {
                    return None;
                }
                let close = self.close(open);
                let items = self.inside(open, close);
                let last = if self.punct(on(close, 1), ";") { on(close, 1) } else { close };
                let name = format!("{}!", self.spoken(i, at.saturating_sub(1)));
                let mut item = self.leaf(head("", name), last);
                item.items = items;
                (!item.items.is_empty()).then_some(item)
            }
        }
    }

    /// An item that holds items (a `mod`, an `impl`, a `trait`, an `extern` block or a `fn`)
    /// whose body opens at `open`; past its close.
    fn block(&mut self, head: Head, open: usize) -> Item {
        let close = self.close(open);
        let items = self.inside(open, close);
        let mut item = self.leaf(head, close);
        item.items = items;
        item
    }

    /// An item that ends at the token `last`; past it.
    fn leaf(&mut self, head: Head, last: usize) -> Item {
        self.pos = on(last, 1);
        let Head { kind, name, vis, first } = head;
        Item { kind, name, vis, start: self.line(first), end: self.line(last), items: Vec::new() }
    }

    /// Where a head from `from` ends: at the `{` of its body, or its `;`. Angle brackets count
    /// here, since a head's `<` and `>` are generics (`-> Box<dyn Fn() -> u8> {`), and a brace
    /// inside them is a const argument's.
    fn head_end(&self, from: usize) -> Stop {
        let mut angles = 0_usize;
        let mut i = from;
        while i < self.end {
            if self.token(i).is_some_and(|t| t.kind == Kind::Punct) {
                match self.text(i) {
                    "{" if angles == 0 => return Stop::Body(i),
                    "(" | "[" | "{" => i = self.close(i),
                    ";" if angles == 0 => return Stop::Last(i),
                    "<" => angles = on(angles, 1),
                    ">" => angles = angles.saturating_sub(1),
                    _ => {}
                }
            }
            i = on(i, 1);
        }
        Stop::Last(self.last())
    }

    /// The `;` that ends a statement-like item from `from`, past every group in it (a struct's
    /// literal, a closure's body); the block's last token when there is none.
    fn semicolon(&self, from: usize) -> usize {
        let mut i = from;
        while i < self.end {
            if self.punct(i, ";") {
                return i;
            }
            if self.opens(i) {
                i = self.close(i);
            }
            i = on(i, 1);
        }
        self.last()
    }

    /// The `>` that closes the `<` at `open`, `->` aside.
    fn angle_close(&self, open: usize) -> usize {
        let mut depth = 0_usize;
        let mut i = open;
        while i < self.end {
            if self.punct(i, "<") {
                depth = on(depth, 1);
            } else if self.punct(i, ">") {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i;
                }
            } else if self.opens(i) {
                i = self.close(i);
            }
            i = on(i, 1);
        }
        self.last()
    }
}

/// The kind a keyword names.
fn keyword_kind(keyword: &str) -> &'static str {
    match keyword {
        "struct" => "struct",
        "enum" => "enum",
        "union" => "union",
        "trait" => "trait",
        "mod" => "mod",
        "const" => "const",
        "static" => "static",
        _ => "fn",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn flat(items: &[Item]) -> Vec<&Item> {
        items.iter().flat_map(|item| std::iter::once(item).chain(flat(&item.items))).collect()
    }

    fn printed(src: &str) -> String {
        let mut out = String::new();
        write_items(&mut out, &outline(src), 0);
        out
    }

    /// What `rg -n '^\s*(pub(\(crate\))? )?(async )?fn '` finds.
    fn searched(line: &str) -> bool {
        let line = line.trim_start();
        let line =
            line.strip_prefix("pub(crate) ").or_else(|| line.strip_prefix("pub ")).unwrap_or(line);
        let line = line.strip_prefix("async ").unwrap_or(line);
        line.starts_with("fn ")
    }

    /// On xtask's own largest files: every function a search for `fn` heads finds is in the
    /// outline from its head's line, and each one with a body ends on its closing brace, at the
    /// head's indent, as rustfmt writes it.
    #[test]
    fn every_function_is_outlined_to_its_closing_brace() {
        let root = camino::Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
        for file in ["src/gate.rs", "src/remote.rs", "src/gate/impact.rs"] {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let items = outline(&text);
            let fns: BTreeMap<usize, &Item> =
                flat(&items).into_iter().filter(|i| i.kind == "fn").map(|i| (i.start, i)).collect();
            let found: Vec<usize> = (1..=lines.len()).filter(|&n| searched(lines[n - 1])).collect();
            assert!(found.len() > 50, "{file}: only {} functions found", found.len());
            // The lines inside a string, which a search cannot tell from code.
            let quoted: Vec<(usize, usize)> = lex(&text)
                .iter()
                .filter(|t| t.kind == Kind::Literal)
                .map(|t| {
                    (
                        t.line,
                        t.line + text.get(t.start..t.end).unwrap_or_default().matches('\n').count(),
                    )
                })
                .collect();
            for &n in &found {
                let in_string = quoted.iter().any(|&(from, to)| from < n && n <= to);
                assert!(fns.contains_key(&n) || in_string, "{file}:{n}: a function it misses");
            }
            for item in fns.values() {
                let (head, last) = (lines[item.start - 1], lines[item.end - 1]);
                if last.trim_end().ends_with(';') && !last.trim_start().starts_with('}') {
                    continue;
                }
                let indent = |l: &str| l.len() - l.trim_start().len();
                let closing = last.trim_start().starts_with('}') && indent(last) == indent(head);
                let one_line = item.start == item.end && last.trim_end().ends_with('}');
                assert!(
                    closing || one_line,
                    "{file}:{} `{}` ends on {}",
                    item.start,
                    item.name,
                    item.end
                );
            }
        }
    }

    /// Braces, quotes and `fn` inside strings, raw strings, chars, byte literals and comments
    /// are not code; a lifetime is not a char's quote.
    #[test]
    fn literals_and_comments_hide_what_they_hold() {
        let src = r##"
/// A doc comment: fn hidden() {
fn a<'a>(x: &'a str) -> char {
    let _ = "}{ fn nope() \" {";
    let _ = r#"} " fn raw() {"#;
    let _ = br"}";
    let _ = ('{', '}', '\'', '\u{7d}', b'{', 'é');
    /* outer /* inner } */ still a comment { */
    'outer: loop { break 'outer; }
    '}'
}
// fn commented() {
fn b() {}
"##;
        assert_eq!(printed(src), "3-11 fn a\n13 fn b\n");
    }

    /// The items of every kind, nested under what holds them, with each one's visibility; the
    /// heads that trip a naive scan (generics with `->`, a const argument in braces, a struct
    /// literal before a `;`) end where they should.
    #[test]
    fn items_nest_under_what_holds_them() {
        let src = r#"#![allow(dead_code)]
use std::fmt;
pub(crate) const LIMIT: Limits = Limits { a: 1, b: [0; 2] };
#[derive(Debug)]
pub struct Limits {
    a: u8,
}
struct Pair(u8, u8);
pub enum Mode { A, B }
static mut COUNT: usize = 0;
type Res<T> = Result<T, ()>;
trait Shape: Fn() -> u8 {
    fn area(&self) -> u8;
    fn sides(&self) -> u8 {
        4
    }
}
impl<T: Into<u8>> fmt::Display for Wrap<T, { 3 }>
where
    T: Copy,
{
    pub async fn show(&self) -> Option<Box<dyn Fn() -> u8>> {
        fn nested() {}
        if true {
            struct Inner;
        }
        None
    }
}
macro_rules! twice {
    ($e:expr) => { $e; $e };
}
thread_local! { static LOCAL: u8 = 0; }
#[cfg(test)]
mod tests {
    unsafe extern "C" {
        fn abs(x: i32) -> i32;
    }
    const fn r#type() -> u8 { 1 }
}
mod outside;
fn body() {
    let p: *const u8 = &raw const X;
    let set = a.union(&b);
    assert!(set.is_empty(), "fn not_an_item() {}");
}
"#;
        assert_eq!(
            printed(src),
            "\
3 pub(crate) const LIMIT
5-7 pub struct Limits
8 struct Pair
9 pub enum Mode
10 static COUNT
11 type Res
12-17 trait Shape
  13 fn area
  14-16 fn sides
18-29 impl fmt::Display for Wrap<T, { 3 }>
  22-28 pub fn show
    23 fn nested
    25 struct Inner
30-32 macro_rules! twice
33 thread_local!
  33 static LOCAL
35-40 mod tests
  36-38 extern \"C\"
    37 fn abs
  39 fn r#type
41 mod outside
42-46 fn body
"
        );
    }
}
