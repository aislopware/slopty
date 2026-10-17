//! slopty: a page's card as assistive tech reads it: a PDF's first page, a CSV's table, the
//! header's actions, and the line it leaves while its page shows in the pane. Each test draws a
//! real card in a test window, its page served by a stand-in host, and judges by the
//! accessibility tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::{NodeId, Role};
use gpui::{Entity, IntoElement, Render, VisualTestContext, Window, div, prelude::*, px};
use slopty_view_doc::{PageCard, PageState};

use super::{PageCardView, PageDoc, PagePaneView, PageTable, PdfDoc};
use crate::live::{self, HostedPage, PageAct, PagePresence};
use crate::theme::Theme;

struct Host(gpui::AnyView);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

struct Both(gpui::AnyView, gpui::AnyView);

impl Render for Both {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.0.clone())
            .child(div().h(px(400.0)).w_full().child(self.1.clone()))
    }
}

#[derive(Debug, Clone)]
struct Node {
    role: Role,
    label: String,
    value: String,
    children: Vec<NodeId>,
}

fn draw(cx: &mut VisualTestContext) -> HashMap<NodeId, Node> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.set_a11y_active(true);
        window.refresh();
        window.draw(cx).clear(cx);
        let Some(update) = window.a11y_tree() else { return HashMap::new() };
        update
            .nodes
            .iter()
            .map(|(id, node)| {
                let node = Node {
                    role: node.role(),
                    label: node.label().unwrap_or_default().to_owned(),
                    value: node.value().unwrap_or_default().to_owned(),
                    children: node.children().to_vec(),
                };
                (*id, node)
            })
            .collect()
    })
}

fn find<'a>(nodes: &'a HashMap<NodeId, Node>, role: Role, label: &str) -> Option<&'a Node> {
    nodes.values().find(|node| node.role == role && node.label == label)
}

fn descendants<'a>(nodes: &'a HashMap<NodeId, Node>, of: &Node, role: Role) -> Vec<&'a Node> {
    let mut found = Vec::new();
    let mut stack: Vec<NodeId> = of.children.clone();
    while let Some(id) = stack.pop() {
        let Some(node) = nodes.get(&id) else { continue };
        if node.role == role {
            found.push(node);
        }
        stack.extend(node.children.iter().copied());
    }
    found
}

/// The stand-in host: the page it serves, and what the card asked of it.
#[derive(Default)]
struct Stand {
    page: Option<HostedPage>,
    acts: Vec<PageAct>,
}

type Shared = Rc<RefCell<Stand>>;

fn serve(cx: &mut gpui::TestAppContext, page: HostedPage) -> Shared {
    let stand: Shared = Rc::new(RefCell::new(Stand { page: Some(page), acts: Vec::new() }));
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let (served, acted) = (stand.clone(), stand.clone());
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        cx.set_reduce_motion(true);
        live::serve_pages(
            cx,
            move |_, _, _| served.borrow().page.clone(),
            move |_, act, _| acted.borrow_mut().acts.push(act),
            |_, _, _| None,
        );
    });
    stand
}

fn hosted(doc: Option<PageDoc>) -> HostedPage {
    HostedPage {
        presence: PagePresence::Warm,
        native: None,
        snapshot: None,
        height: None,
        failed: None,
        doc,
    }
}

fn card(kind: &str, page: &str, title: &str) -> Arc<PageCard> {
    Arc::new(PageCard {
        id: format!("{page}:page"),
        page: page.into(),
        title: title.into(),
        kind: kind.into(),
        source: "/work/out/file".into(),
        ..PageCard::default()
    })
}

fn open_card(
    cx: &mut gpui::TestAppContext,
    card: Arc<PageCard>,
) -> (Entity<PageCardView>, &mut VisualTestContext) {
    let view = cx.new(|cx| PageCardView::new(card, Some("chat".into()), None, cx));
    let shown = view.clone();
    let (_, cx) = cx.add_window_view(|_, _| Host(shown.into()));
    cx.simulate_resize(gpui::size(px(900.0), px(800.0)));
    (view, cx)
}

/// A PDF's card shows its first page, an image node "page 1 of 3": loading until its picture
/// comes, which the card asks the host to draw at the width it shows it.
#[gpui::test]
fn a_pdf_shows_its_first_page_and_names_its_count(cx: &mut gpui::TestAppContext) {
    let pdf = PdfDoc { pages: 3, aspect: 1.294, drawn: HashMap::new() };
    let stand = serve(cx, hosted(Some(PageDoc::Pdf(Arc::new(pdf.clone())))));
    let (_, cx) = open_card(cx, card(PageCard::FILE, "slopty-page:1/pdf", "report.pdf"));
    let nodes = draw(cx);
    let page = find(&nodes, Role::Image, "page 1 of 3").expect("the first page's node");
    assert_eq!(page.value, "loading", "not drawn yet");
    let asked: Vec<(usize, f32)> = stand
        .borrow()
        .acts
        .iter()
        .filter_map(|act| match act {
            PageAct::Draw { index, width } => Some((*index, *width)),
            _ => None,
        })
        .collect();
    assert!(asked.iter().all(|(index, _)| *index == 0), "only the first page: {asked:?}");
    let (_, width) = *asked.last().expect("it asks for the first page");
    assert!(width > 100.0 && width <= 900.0, "drawn as wide as it shows: {width}");

    let picture = Arc::new(gpui::RenderImage::new([image::Frame::new(image::RgbaImage::new(
        10, 13,
    ))]));
    let mut drawn = pdf;
    drawn.drawn.insert(0, (width, picture));
    stand.borrow_mut().page = Some(hosted(Some(PageDoc::Pdf(Arc::new(drawn)))));
    let nodes = draw(cx);
    let page = find(&nodes, Role::Image, "page 1 of 3").expect("the first page's node");
    assert_eq!(page.value, "drawn");
    assert!(find(&nodes, Role::Image, "page 2 of 3").is_none(), "the card shows one page");
}

/// A CSV of ten records shows as a table node: a header row of column headers and ten rows of
/// cells, each cell named by its text. Past the card's rows it says how many more there are.
#[gpui::test]
fn a_csv_shows_as_a_table_of_its_rows(cx: &mut gpui::TestAppContext) {
    let mut text = String::from("region,p99\n");
    for n in 0..10 {
        text.push_str(&format!("r{n},{}\n", 100 + n));
    }
    let table = PageTable::parse(&text, ',');
    serve(cx, hosted(Some(PageDoc::Table(Arc::new(table)))));
    let (_, cx) = open_card(cx, card(PageCard::FILE, "slopty-page:1/csv", "latency.csv"));
    let nodes = draw(cx);
    let table = find(&nodes, Role::Table, "latency.csv").expect("the table's node");
    let rows = descendants(&nodes, table, Role::Row);
    let with_cells = rows.iter().filter(|row| !descendants(&nodes, row, Role::Cell).is_empty());
    assert_eq!(with_cells.count(), 10, "a row per record");
    assert_eq!(rows.len(), 11, "and the header's");
    assert_eq!(descendants(&nodes, table, Role::ColumnHeader).len(), 2);
    assert!(find(&nodes, Role::Cell, "r9").is_some(), "the last record");
    assert!(find(&nodes, Role::Button, "Copy table").is_some());
    assert!(!nodes.values().any(|n| n.label.contains("more row")), "all ten show");
    let group = nodes.values().find(|n| n.role == Role::Group && n.label.ends_with("latency.csv"));
    assert_eq!(group.map(|n| n.value.as_str()), Some("live"), "drawn here, the card is live");
}

/// The header carries what the page's kind can do: a web page copies its picture, saves, moves
/// to the pane and expands; a table copies as text.
#[gpui::test]
fn a_card_offers_what_its_page_can_do(cx: &mut gpui::TestAppContext) {
    serve(cx, hosted(None));
    let (_, cx) = open_card(cx, card(PageCard::HTML, "slopty-page:1/html", "Plot"));
    let nodes = draw(cx);
    let buttons: Vec<&str> = ["Copy image", "Save", "Open in pane", "Expand page", "Copy table"]
        .into_iter()
        .filter(|label| find(&nodes, Role::Button, label).is_some())
        .collect();
    assert_eq!(buttons, ["Copy image", "Save", "Open in pane", "Expand page"]);
}

/// While its page shows in the pane, the card keeps one line, "Shown in the pane", with Bring
/// back, and offers nothing else; the pane names the page and says it is the pane.
#[gpui::test]
fn a_page_in_the_pane_leaves_a_line_in_its_card(cx: &mut gpui::TestAppContext) {
    serve(cx, hosted(None));
    let pane = cx.new(|cx| {
        PagePaneView::new("slopty-page:1/html".into(), "Plot".into(), "chat".into(), None, cx)
    });
    let card = card(PageCard::HTML, "slopty-page:1/html", "Plot");
    let view = cx.new(|cx| PageCardView::new(card, Some("chat".into()), None, cx));
    let (shown, panes) = (view.clone(), pane.clone());
    let (_, cx) = cx.add_window_view(|_, _| Both(shown.into(), panes.into()));
    cx.simulate_resize(gpui::size(px(900.0), px(800.0)));
    view.update(cx, |view, cx| {
        view.state = Some(PageState {
            page: "slopty-page:1/html".into(),
            presence: PageState::LIVE.into(),
            display_mode: "pane".into(),
            ..PageState::default()
        });
        cx.notify();
    });
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Group, "Shown in the pane").is_some(), "the card's line");
    assert!(find(&nodes, Role::Button, "Bring back").is_some());
    assert!(find(&nodes, Role::Button, "Open in pane").is_none(), "it is there already");
    let pane = nodes.values().find(|n| n.role == Role::Group && n.label == "Page: Plot" && n.value == "pane");
    assert!(pane.is_some(), "the pane's node, beside the card's");
    assert!(find(&nodes, Role::Button, "Back to the chat").is_some());
}

/// A page that could not be shown says why with Try again, which asks its host to open it
/// afresh; a page whose call failed says why and offers no try, which could go no otherwise.
#[gpui::test]
fn a_failed_page_offers_try_again(cx: &mut gpui::TestAppContext) {
    let stand = serve(cx, HostedPage { failed: Some("The page stopped".into()), ..hosted(None) });
    let (view, cx) = open_card(cx, card(PageCard::HTML, "slopty-page:1/html", "Plot"));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Try again").is_some(), "the way to ask again");
    let bounds = cx.debug_bounds("slopty-page:1/html-try-again").expect("Try again is drawn");
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    assert!(stand.borrow().acts.contains(&PageAct::Reload), "{:?}", stand.borrow().acts);

    view.update(cx, |view, cx| {
        view.card = Arc::new(PageCard { failed: true, ..(*view.card).clone() });
        cx.notify();
    });
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Try again").is_none(), "a failed call is not tried again");
}


struct Moved(gpui::AnyView, f32);

impl Render for Moved {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div().size_full().pt(px(self.1)).child(self.0.clone())
    }
}

/// A live page moved into a fade band by what is around it rests in the frames the window
/// draws, though nothing told the card itself: a frame built from scratch, which rests it, draws
/// nothing the shown one does not.
#[gpui::test]
fn a_live_page_moved_out_of_its_band_rests_in_the_frame_shown(cx: &mut gpui::TestAppContext) {
    let stand = serve(cx, HostedPage { presence: PagePresence::Live, ..hosted(None) });
    let view = cx.new(|cx| {
        PageCardView::new(card(PageCard::HTML, "slopty-page:1/html", "Plot"), None, None, cx)
    });
    let shown = view.clone();
    let (host, cx) = cx.add_window_view(|_, _| Moved(shown.into(), 200.0));
    cx.simulate_resize(gpui::size(px(900.0), px(800.0)));
    view.update(cx, |view, _| view.set_clear_band(100.0, 790.0));
    let frame = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    };
    frame(cx);
    frame(cx);
    let rests = |stand: &Shared| stand.borrow().acts.iter().filter(|a| **a == PageAct::Rest).count();
    assert_eq!(rests(&stand), 0, "in its band it stays live: {:?}", stand.borrow().acts);

    host.update(cx, |host, cx| {
        host.1 = 20.0;
        cx.notify();
    });
    frame(cx);
    frame(cx);
    assert!(rests(&stand) > 0, "under the titlebar's fade it rests: {:?}", stand.borrow().acts);
}
