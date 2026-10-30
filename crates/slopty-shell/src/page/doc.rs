//! Files a page card draws itself rather than in a web view: a PDF's pages, drawn by `PDFKit` in
//! the app, and a CSV or TSV file's table.
//!
//! A PDF shows its first page in the card, fitted whole within the inline cap, labelled
//! "page 1 of N"; the pane shows every page. A table shows its first rows in the card in the
//! markdown table's chrome (hairlines between rows, the header at weight 700), and every row in
//! the pane. Both are a11y nodes of their own: an image per page, a table of rows and cells.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, FontWeight, ObjectFit, RenderImage, SharedString, div, img, px};

use crate::markdown::render::{
    MD_LINE_HEIGHT, MD_TEXT_SIZE, TABLE_CELL_PADDING, TABLE_DIVIDER, TABLE_HEADER_WEIGHT,
    TableColumns, table_columns, table_hairline,
};
use crate::theme::Theme;

/// What a page is when the app draws it itself.
#[derive(Clone, Debug)]
pub enum PageDoc {
    /// A PDF, as far as it is drawn.
    Pdf(Arc<PdfDoc>),
    /// A CSV or TSV file's table.
    Table(Arc<PageTable>),
}

/// A PDF, as far as the app has drawn it.
#[derive(Clone, Debug, Default)]
pub struct PdfDoc {
    /// How many pages it has.
    pub pages: usize,
    /// Its first page's height over its width, which every page not yet drawn takes.
    pub aspect: f32,
    /// The pages drawn, by index (from 0): the width in points each was drawn for, and its
    /// picture.
    pub drawn: HashMap<usize, (f32, Arc<RenderImage>)>,
}

impl PdfDoc {
    /// Page `index` drawn for `width` points, within a tenth of it; `None` when it has to be
    /// drawn (again).
    #[must_use]
    pub fn picture(&self, index: usize, width: f32) -> Option<Arc<RenderImage>> {
        let (drawn, picture) = self.drawn.get(&index)?;
        ((drawn - width).abs() <= width * 0.1).then(|| Arc::clone(picture))
    }

    /// The best picture of page `index` there is, whatever width it was drawn for.
    #[must_use]
    pub fn any_picture(&self, index: usize) -> Option<Arc<RenderImage>> {
        self.drawn.get(&index).map(|(_, picture)| Arc::clone(picture))
    }

    /// Page `index`'s height over its width.
    #[must_use]
    pub fn aspect_of(&self, index: usize) -> f32 {
        self.drawn
            .get(&index)
            .map_or(self.aspect, |(_, picture)| {
                let size = picture.size(0);
                let side = |pixels: i32| u16::try_from(pixels).map_or(0.0, f32::from);
                let (w, h) = (side(size.width.0), side(size.height.0));
                if w > 0.0 { h / w } else { self.aspect }
            })
            .max(0.05)
    }
}

/// What a page's PDF says of its pages to assistive technologies: "page 1 of 3".
#[must_use]
pub fn page_words(index: usize, pages: usize) -> String {
    format!("page {} of {pages}", index.saturating_add(1))
}

/// A table read from a CSV or TSV file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageTable {
    /// Its first record, which names the columns.
    pub header: Vec<SharedString>,
    /// The records after it, at most [`PageTable::MAX_ROWS`].
    pub rows: Vec<Vec<SharedString>>,
    /// Records the file has past those kept.
    pub more: usize,
    /// Each column's widest text, estimated from its characters, as the markdown table sizes
    /// its columns by their content.
    pub content: Vec<f32>,
}

impl PageTable {
    /// The records the card shows; the pane shows them all.
    pub const INLINE_ROWS: usize = 12;
    /// The most characters a cell keeps.
    pub const MAX_CELL: usize = 400;
    /// The most columns kept.
    pub const MAX_COLUMNS: usize = 64;
    /// The most records kept, the header aside.
    pub const MAX_ROWS: usize = 10_000;

    /// The delimiter a file's name says its records use: `,` for `.csv`, a tab for `.tsv`.
    #[must_use]
    pub fn delimiter_of(path: &str) -> Option<char> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Some(','),
            "tsv" | "tab" => Some('\t'),
            _ => None,
        }
    }

    /// `text`'s records, split by `delimiter` as RFC 4180 reads them: a field in double quotes
    /// may hold the delimiter, a line break and `""` for a quote; a line ends with LF or CRLF.
    /// The first record is the header; blank lines are skipped.
    #[must_use]
    pub fn parse(text: &str, delimiter: char) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut records: Vec<Vec<SharedString>> = Vec::new();
        let mut more = 0_usize;
        let mut record: Vec<String> = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut chars = text.chars().peekable();
        let mut end_record = |record: &mut Vec<String>, field: &mut String, more: &mut usize| {
            let blank = record.is_empty() && field.is_empty();
            record.push(std::mem::take(field));
            let done = std::mem::take(record);
            if blank {
                return;
            }
            if records.len() > Self::MAX_ROWS {
                *more = more.saturating_add(1);
                return;
            }
            records.push(
                done.into_iter()
                    .take(Self::MAX_COLUMNS)
                    .map(|cell| {
                        SharedString::from(cell.chars().take(Self::MAX_CELL).collect::<String>())
                    })
                    .collect(),
            );
        };
        while let Some(c) = chars.next() {
            if quoted {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        quoted = false;
                    }
                } else {
                    field.push(c);
                }
                continue;
            }
            match c {
                '"' if field.is_empty() => quoted = true,
                '\r' if chars.peek() == Some(&'\n') => {}
                '\n' | '\r' => end_record(&mut record, &mut field, &mut more),
                c if c == delimiter => record.push(std::mem::take(&mut field)),
                c => field.push(c),
            }
        }
        if !record.is_empty() || !field.is_empty() {
            end_record(&mut record, &mut field, &mut more);
        }
        let columns = records.iter().map(Vec::len).max().unwrap_or(0);
        let mut content = vec![0.0_f32; columns];
        for record in &records {
            for (width, cell) in content.iter_mut().zip(record) {
                // About 7 points a character of the 14-point text, at most a wide column.
                let chars = cell.chars().count().min(80);
                *width = width.max(f32::from(u16::try_from(chars).unwrap_or(80)) * 7.0);
            }
        }
        let mut records = records.into_iter();
        let header = records.next().unwrap_or_default();
        Self { header, rows: records.collect(), more, content }
    }

    /// Its columns' geometry, as the markdown table's.
    #[must_use]
    pub fn geometry(&self) -> TableColumns {
        table_columns(&self.content)
    }

    /// The table as tab-separated text, as a spreadsheet pastes it.
    #[must_use]
    pub fn to_tsv(&self) -> String {
        std::iter::once(&self.header)
            .chain(self.rows.iter())
            .map(|record| {
                record
                    .iter()
                    .map(|cell| cell.replace(['\t', '\n'], " "))
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// How many columns it has.
    #[must_use]
    pub fn columns(&self) -> usize {
        std::iter::once(&self.header).chain(self.rows.iter()).map(Vec::len).max().unwrap_or(0)
    }
}

/// A PDF's first page in the card: drawn whole within `cap` points high and `width` wide,
/// centred on the transcript's surface, an image node "page 1 of N".
pub fn pdf_first_page(
    id: SharedString,
    doc: &PdfDoc,
    width: f32,
    cap: f32,
    theme: &Theme,
) -> AnyElement {
    let aspect = doc.aspect_of(0);
    let shown = fit(width, cap, aspect);
    let picture = doc.picture(0, shown.0).or_else(|| doc.any_picture(0));
    div()
        .w_full()
        .flex()
        .justify_center()
        .child(page_image(id, picture, shown, 0, doc.pages, theme))
        .into_any_element()
}

/// `width` × (`width` × `aspect`), shrunk to `cap` high.
#[must_use]
pub fn fit(width: f32, cap: f32, aspect: f32) -> (f32, f32) {
    let height = width * aspect;
    if height <= cap || height <= 0.0 { (width, height) } else { (cap / aspect, cap) }
}

/// One page of a PDF, `(width, height)` points: its picture once drawn, the paper's place while
/// it draws.
pub fn page_image(
    id: SharedString,
    picture: Option<Arc<RenderImage>>,
    (width, height): (f32, f32),
    index: usize,
    pages: usize,
    theme: &Theme,
) -> AnyElement {
    let words = SharedString::from(page_words(index, pages));
    let frame = div()
        .id(id)
        .role(gpui::Role::Image)
        .aria_label(words)
        .flex_none()
        .w(px(width))
        .h(px(height))
        .rounded(px(4.0))
        .overflow_hidden()
        .border_1()
        .border_color(crate::theme::hairline(0.07));
    match picture {
        Some(picture) => frame
            .aria_value("drawn")
            .child(img(gpui::ImageSource::Render(picture)).size_full().object_fit(ObjectFit::Fill))
            .into_any_element(),
        None => frame.aria_value("loading").bg(theme.ink(0.04)).into_any_element(),
    }
}

/// A row of a table in the markdown table's chrome: a row node, a cell node per field (a column
/// header in the header row).
pub fn table_row(
    id: &str,
    ix: usize,
    record: &[SharedString],
    header: bool,
    geometry: &TableColumns,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!("{id}-row-{ix}")))
        .role(gpui::Role::Row)
        .flex()
        .flex_row()
        .w_full()
        .children(geometry.naturals.iter().zip(&geometry.minimums).enumerate().map(
            |(c, (natural, minimum))| {
                let text = record.get(c).cloned().unwrap_or_default();
                div()
                    .id(SharedString::from(format!("{id}-cell-{ix}-{c}")))
                    .role(if header { gpui::Role::ColumnHeader } else { gpui::Role::Cell })
                    .aria_label(text.clone())
                    .flex_grow(*natural)
                    .flex_shrink(*natural)
                    .flex_basis(px(0.0))
                    .min_w(px(*minimum))
                    .p(px(TABLE_CELL_PADDING))
                    .text_size(crate::typography::ui_rems(MD_TEXT_SIZE))
                    .line_height(crate::typography::ui_rems(MD_LINE_HEIGHT))
                    .text_color(theme.text)
                    .font_weight(if header { TABLE_HEADER_WEIGHT } else { FontWeight::NORMAL })
                    .truncate()
                    .child(text)
            },
        ))
}

/// The height of one row of a table, its hairline above it included.
pub const TABLE_ROW_HEIGHT: f32 = MD_LINE_HEIGHT + 2.0 * TABLE_CELL_PADDING + TABLE_DIVIDER;

/// A table's first `limit` rows in the markdown table's chrome.
///
/// That is a table node labelled `label`, a row node per record (the header first), a cell node
/// per field. It scrolls sideways when its columns' floors do not fit, as the markdown table does.
pub fn table(
    id: &str,
    label: SharedString,
    table: &PageTable,
    limit: usize,
    theme: &Theme,
) -> AnyElement {
    let geometry = table.geometry();
    let hairline = table_hairline();
    let mut inner = div().flex().flex_col().w_full().min_w(px(geometry.min_table_width));
    if !table.header.is_empty() {
        inner = inner.child(table_row(id, 0, &table.header, true, &geometry, theme));
    }
    for (ix, record) in table.rows.iter().take(limit).enumerate() {
        inner = inner
            .child(div().flex_none().h(px(TABLE_DIVIDER)).w_full().bg(hairline))
            .child(table_row(id, ix.saturating_add(1), record, false, &geometry, theme));
    }
    div()
        .id(SharedString::from(format!("{id}-table")))
        .role(gpui::Role::Table)
        .aria_label(label)
        .w_full()
        .overflow_x_scroll()
        .child(inner)
        .into_any_element()
}

/// What a table's card says of the rows it does not show.
#[must_use]
pub fn more_rows_words(table: &PageTable, shown: usize) -> Option<String> {
    let hidden = table.rows.len().saturating_sub(shown).saturating_add(table.more);
    match hidden {
        0 => None,
        1 => Some("1 more row".to_owned()),
        n => Some(format!("{n} more rows")),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// RFC 4180: quoted fields keep delimiters, line breaks and doubled quotes; CRLF and a BOM
    /// are read; blank lines are skipped; a TSV splits on tabs.
    #[test]
    fn a_csv_reads_as_rfc_4180_says() {
        let text = "\u{feff}name,note\r\nada,\"says \"\"hi\"\", then, leaves\"\r\n\r\nbob,\"two\nlines\"\n";
        let table = PageTable::parse(text, ',');
        assert_eq!(table.header, ["name", "note"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][1], "says \"hi\", then, leaves");
        assert_eq!(table.rows[1][1], "two\nlines");
        let tsv = PageTable::parse("a\tb\n1\t2,3\n", '\t');
        assert_eq!(tsv.rows, [vec![SharedString::from("1"), SharedString::from("2,3")]]);
        assert_eq!(PageTable::delimiter_of("/w/out/data.TSV"), Some('\t'));
        assert_eq!(PageTable::delimiter_of("/w/out/data.csv"), Some(','));
        assert_eq!(PageTable::delimiter_of("/w/out/data.json"), None);
    }

    /// Past the row cap the rest is counted, not kept; the card names what it does not show.
    #[test]
    fn a_long_table_counts_what_it_does_not_keep() {
        let mut text = String::from("n\n");
        for n in 0..PageTable::MAX_ROWS + 5 {
            writeln!(text, "{n}").unwrap();
        }
        let table = PageTable::parse(&text, ',');
        assert_eq!(table.rows.len(), PageTable::MAX_ROWS);
        assert_eq!(table.more, 5);
        assert_eq!(
            more_rows_words(&table, 12),
            Some(format!("{} more rows", PageTable::MAX_ROWS - 12 + 5))
        );
        let small = PageTable::parse("a\n1\n", ',');
        assert_eq!(more_rows_words(&small, 12), None);
        assert_eq!(small.to_tsv(), "a\n1");
    }

    /// A page fits the card whole: wide pages at the card's width, tall ones shrunk to the cap.
    #[test]
    fn a_page_fits_within_the_cap() {
        assert_eq!(fit(600.0, 720.0, 0.5), (600.0, 300.0));
        let (w, h) = fit(600.0, 720.0, 1.414);
        assert!((h - 720.0).abs() < 1e-3 && (w - 720.0 / 1.414).abs() < 1e-3);
        assert_eq!(page_words(0, 3), "page 1 of 3");
    }
}
