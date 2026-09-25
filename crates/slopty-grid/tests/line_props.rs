//! A line leaves its trailing blank cells off the wire; whatever the cells, it decodes to the
//! line it was.

#[cfg(test)]
mod props {
    use proptest::prelude::*;
    use slopty_grid::{Cell, CellText, CellWidth, Color, Hyperlink, Line, SemanticMark, Style};

    fn cell() -> impl Strategy<Value = Cell> {
        prop_oneof![
            // Blank cells are common, and the trimming is about them.
            4 => Just(Cell::BLANK),
            2 => proptest::char::range(' ', '~').prop_map(|c| Cell::narrow(c, Style::DEFAULT)),
            1 => (any::<u8>(), 0_u8..3).prop_map(|(i, width)| Cell {
                text: CellText::EMPTY,
                style: Style { bg: Color::Palette(i), ..Style::DEFAULT },
                width: [CellWidth::Narrow, CellWidth::SpacerTail, CellWidth::SpacerHead]
                    [usize::from(width)],
            }),
            1 => Just(Cell::wide("字", Style::DEFAULT)),
        ]
    }

    fn line() -> impl Strategy<Value = Line> {
        (
            proptest::collection::vec(cell(), 0..120),
            any::<bool>(),
            proptest::option::of(any::<u8>()),
            proptest::option::of((0_u16..100, 1_u16..20)),
        )
            .prop_map(|(cells, wrapped, exit, link)| {
                let mut line = Line { cells, ..Line::default() };
                line.flags.set(slopty_grid::LineFlags::WRAPPED, wrapped);
                line.mark = SemanticMark::Prompt { exit, input: None };
                line.links = link
                    .map(|(col, len)| Hyperlink { col, len, uri: "https://a.b".to_owned() })
                    .into_iter()
                    .collect();
                line
            })
    }

    proptest! {
        #[test]
        fn a_line_round_trips_without_its_trailing_blanks(line in line()) {
            let json = serde_json::to_value(&line).expect("encodes");
            let sent = json["cells"].as_array().map_or(0, Vec::len);
            let content = line.last_content_col().map_or(0, |c| usize::from(c) + 1);
            prop_assert_eq!(sent, content);
            prop_assert_eq!(serde_json::from_value::<Line>(json).expect("decodes"), line);
        }
    }
}
