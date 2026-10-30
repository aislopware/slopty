// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest as _, Sha256};

use crate::{
    AccentRoles, Appearance, Color, SurfaceTreatment, TerminalPalette, ThemeColors, ThemeFamily,
    ThemeRegistry, ThemeSource, ThemeVariant,
};

// A curated few (Amp's lesson): the house pair, a true black, a high-contrast pair, and
// four strong colourful ports, each with one line of voice (`docs/decisions/ui.md`).
/// The built-in themes, built once.
pub fn builtin_registry() -> &'static ThemeRegistry {
    static REGISTRY: OnceLock<ThemeRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| ThemeRegistry {
        families: vec![
            family("slopty", "Slopty", vec![slopty_light(), slopty_dark(), slopty_black()]),
            family(
                "high-contrast",
                "High Contrast",
                vec![high_contrast_light(), high_contrast_dark()],
            ),
            family("tokyo-night", "Tokyo Night", vec![tokyo_dark()]),
            family("catppuccin", "Catppuccin", vec![catppuccin_mocha()]),
            family("dracula", "Dracula", vec![dracula()]),
            family("rose-pine", "Rosé Pine", vec![rose_pine_dawn()]),
        ],
    })
}

/// The id of the house's own variant for `appearance`, which a theme no longer built in resolves
/// to.
pub(crate) const fn house_id(appearance: Appearance) -> &'static str {
    if appearance.is_dark() { "slopty-dark" } else { "slopty-light" }
}

/// The house's own variant for `appearance`, for a registry that lacks it.
pub(crate) fn house(appearance: Appearance) -> &'static ThemeVariant {
    static HOUSE: OnceLock<[ThemeVariant; 2]> = OnceLock::new();
    let [light, dark] = HOUSE.get_or_init(|| [slopty_light(), slopty_dark()]);
    if appearance.is_dark() { dark } else { light }
}

fn family(id: &str, name: &str, variants: Vec<ThemeVariant>) -> ThemeFamily {
    ThemeFamily { id: id.into(), name: name.into(), variants }
}

struct Seeds<'a> {
    id: &'a str,
    family_id: &'a str,
    name: &'a str,
    description: &'a str,
    appearance: Appearance,
    treatment: SurfaceTreatment,
    background: Color,
    shell: Color,
    raised: Color,
    card: Color,
    text: Color,
    muted: Color,
    faint: Color,
    accent: Color,
    danger: Color,
    warning: Color,
    success: Color,
    terminal_background: Color,
    ansi: [Color; 16],
    syntax: [Color; 12],
    source: ThemeSource,
}

fn variant(seed: Seeds<'_>) -> ThemeVariant {
    let background = seed.background;
    let shell = seed.shell;
    let raised = seed.raised;
    let card = seed.card;
    let text = seed.text;
    let muted = seed.muted.ensure_contrast(background, 4.5);
    let faint = seed.faint;
    let danger = seed.danger;
    let warning = seed.warning;
    let success = seed.success;
    let accent = AccentRoles::derive(seed.accent, seed.appearance, background);
    let dark = seed.appearance.is_dark();
    let border_tone = if dark { Color::WHITE } else { Color::BLACK };
    let solid = if dark { Color::rgb(235, 235, 239) } else { Color::rgb(35, 35, 40) };
    let colors = ThemeColors {
        background,
        shell,
        raised,
        card,
        dialog: card.mix(raised, if dark { 0.18 } else { 0.04 }),
        overlay: card.mix(raised, if dark { 0.34 } else { 0.02 }),
        hover: border_tone.with_alpha(if dark { 0.11 } else { 0.06 }),
        active: accent.primary.with_alpha(if dark { 0.18 } else { 0.10 }),
        border: border_tone.with_alpha(if dark { 0.10 } else { 0.12 }),
        border_strong: border_tone.with_alpha(if dark { 0.18 } else { 0.22 }),
        text,
        text_muted: muted,
        text_faint: faint,
        solid,
        on_solid: solid.best_on_color(),
        danger,
        danger_muted: danger.mix(text, 0.28),
        warning,
        warning_muted: warning.mix(text, 0.25),
        success,
        success_muted: success.mix(text, 0.25),
        input: if dark { raised.with_alpha(0.72) } else { card },
        cursor: text.with_alpha(if dark { 0.40 } else { 0.55 }),
        diff_add: success,
        diff_delete: danger,
    };
    let terminal_background = seed.terminal_background;
    let mut variant = ThemeVariant {
        id: seed.id.into(),
        family_id: seed.family_id.into(),
        name: seed.name.into(),
        description: seed.description.into(),
        appearance: seed.appearance,
        recommended_surface_treatment: seed.treatment,
        colors,
        accent,
        syntax: syntax(seed.syntax),
        terminal: TerminalPalette {
            background: terminal_background,
            foreground: text.ensure_contrast(terminal_background, 4.5),
            selection: border_tone.with_alpha(if dark { 0.22 } else { 0.16 }),
            ansi: seed.ansi,
        },
        source: seed.source,
    };
    seal(&mut variant);
    variant
}

/// Hash the checked-in resolved definition itself (with the hash field blanked), not merely its
/// source URL. This makes provenance sensitive to curation edits as well as upstream revision
/// changes.
fn seal(variant: &mut ThemeVariant) {
    variant.source.asset_hash.clear();
    // A variant serializes whole: every map key is a string, so nothing here can fail.
    let encoded = serde_json::to_vec(&variant).unwrap_or_default();
    variant.source.asset_hash =
        format!("sha256:{}", data_encoding::HEXLOWER.encode(&Sha256::digest(encoded)));
}

fn syntax(colors: [Color; 12]) -> BTreeMap<String, Color> {
    let [
        comment,
        keyword,
        string,
        number,
        type_name,
        function,
        property,
        variable,
        punctuation,
        tag,
        attribute,
        invalid,
    ] = colors;
    BTreeMap::from([
        ("comment".into(), comment),
        ("keyword".into(), keyword),
        ("string".into(), string),
        ("stringSpecial".into(), attribute),
        ("escape".into(), attribute),
        ("number".into(), number),
        ("boolean".into(), number),
        ("type".into(), type_name),
        ("typeBuiltin".into(), type_name),
        ("constructor".into(), type_name),
        ("function".into(), function),
        ("functionBuiltin".into(), function),
        ("macro".into(), keyword),
        ("property".into(), property),
        ("constant".into(), number),
        ("variable".into(), variable),
        ("variableSpecial".into(), keyword),
        ("parameter".into(), variable),
        ("operator".into(), keyword),
        ("punctuation".into(), punctuation),
        ("tag".into(), tag),
        ("attribute".into(), attribute),
        ("label".into(), function),
        ("embedded".into(), punctuation),
        ("invalid".into(), invalid),
    ])
}

fn source(id: &str, format: &str, url: &str, revision: &str, license: &str) -> ThemeSource {
    ThemeSource {
        format: format.into(),
        url: url.into(),
        revision: revision.into(),
        license: license.into(),
        asset_hash: format!("pending:{id}"),
    }
}

const ANSI_DARK: [Color; 16] = [
    hex!("#242424"),
    hex!("#f87171"),
    hex!("#4ade80"),
    hex!("#facc15"),
    hex!("#60a5fa"),
    hex!("#c084fc"),
    hex!("#22d3ee"),
    hex!("#d4d4d8"),
    hex!("#52525b"),
    hex!("#fca5a5"),
    hex!("#86efac"),
    hex!("#fde047"),
    hex!("#93c5fd"),
    hex!("#d8b4fe"),
    hex!("#67e8f9"),
    hex!("#fafafa"),
];

const ANSI_LIGHT: [Color; 16] = [
    hex!("#1f1f1f"),
    hex!("#dc2626"),
    hex!("#16a34a"),
    hex!("#b45309"),
    hex!("#2563eb"),
    hex!("#9333ea"),
    hex!("#0e7490"),
    hex!("#3f3f46"),
    hex!("#71717a"),
    hex!("#b91c1c"),
    hex!("#15803d"),
    hex!("#92400e"),
    hex!("#1d4ed8"),
    hex!("#7e22ce"),
    hex!("#155e75"),
    hex!("#18181b"),
];

fn slopty_dark() -> ThemeVariant {
    variant(Seeds {
        id: "slopty-dark",
        family_id: "slopty",
        name: "Slopty Dark",
        description: "Soft greys under a violet accent.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Frosted,
        background: hex!("#060606"),
        shell: hex!("#0d0d0d"),
        raised: hex!("#343438"),
        card: hex!("#0e0e0e"),
        text: hex!("#e8e8ea"),
        muted: hex!("#a9a9ae"),
        faint: hex!("#85858a"),
        accent: hex!("#8b7cf6"),
        danger: hex!("#f87171"),
        warning: hex!("#facc15"),
        success: hex!("#34d399"),
        terminal_background: hex!("#090909"),
        ansi: ANSI_DARK,
        syntax: [
            hex!("#92929a"),
            hex!("#8b7cf6"),
            hex!("#34d399"),
            hex!("#facc15"),
            hex!("#c084fc"),
            hex!("#60a5fa"),
            hex!("#f472b6"),
            hex!("#e8e8ea"),
            hex!("#a1a1aa"),
            hex!("#f472b6"),
            hex!("#22d3ee"),
            hex!("#f87171"),
        ],
        source: source(
            "slopty-dark",
            "native",
            "https://github.com/aislopware/slopty",
            "d138049",
            "MIT",
        ),
    })
}

fn slopty_light() -> ThemeVariant {
    variant(Seeds {
        id: "slopty-light",
        family_id: "slopty",
        name: "Slopty Light",
        description: "Paper white under the same violet.",
        appearance: Appearance::Light,
        treatment: SurfaceTreatment::Frosted,
        background: hex!("#ffffff"),
        shell: hex!("#f3f3f5"),
        raised: hex!("#ededf0"),
        card: hex!("#ffffff"),
        text: hex!("#303035"),
        muted: hex!("#62626a"),
        faint: hex!("#797981"),
        accent: hex!("#5b43e8"),
        danger: hex!("#dc2626"),
        warning: hex!("#a16207"),
        success: hex!("#15803d"),
        terminal_background: hex!("#fafafa"),
        ansi: ANSI_LIGHT,
        syntax: [
            hex!("#6b7280"),
            hex!("#5b43e8"),
            hex!("#15803d"),
            hex!("#a16207"),
            hex!("#7e22ce"),
            hex!("#2563eb"),
            hex!("#be185d"),
            hex!("#303035"),
            hex!("#52525b"),
            hex!("#be185d"),
            hex!("#0e7490"),
            hex!("#b91c1c"),
        ],
        source: source(
            "slopty-light",
            "native",
            "https://github.com/aislopware/slopty",
            "d138049",
            "MIT",
        ),
    })
}

fn slopty_black() -> ThemeVariant {
    variant(Seeds {
        id: "slopty-black",
        family_id: "slopty",
        name: "Slopty Black",
        description: "True black, for OLED screens and late nights.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#000000"),
        shell: hex!("#000000"),
        raised: hex!("#2a2a2e"),
        card: hex!("#050505"),
        text: hex!("#ededf0"),
        muted: hex!("#a9a9ae"),
        faint: hex!("#85858a"),
        accent: hex!("#8b7cf6"),
        danger: hex!("#f87171"),
        warning: hex!("#facc15"),
        success: hex!("#34d399"),
        terminal_background: hex!("#000000"),
        ansi: ANSI_DARK,
        syntax: [
            hex!("#92929a"),
            hex!("#8b7cf6"),
            hex!("#34d399"),
            hex!("#facc15"),
            hex!("#c084fc"),
            hex!("#60a5fa"),
            hex!("#f472b6"),
            hex!("#ededf0"),
            hex!("#a1a1aa"),
            hex!("#f472b6"),
            hex!("#22d3ee"),
            hex!("#f87171"),
        ],
        source: source(
            "slopty-black",
            "native",
            "https://github.com/aislopware/slopty",
            "d138049",
            "MIT",
        ),
    })
}

/// A high-contrast variant draws its edges in the text's own ink, strong enough to read
/// without colour, as VS Code's high-contrast themes do.
fn high_contrast(mut variant: ThemeVariant) -> ThemeVariant {
    let ink = variant.colors.text;
    variant.colors.border = ink.with_alpha(0.55);
    variant.colors.border_strong = ink.with_alpha(0.85);
    variant.colors.hover = ink.with_alpha(0.16);
    seal(&mut variant);
    variant
}

// After VS Code's Dark High Contrast (MIT): white on black, cyan edges, orange focus.
fn high_contrast_dark() -> ThemeVariant {
    high_contrast(variant(Seeds {
        id: "high-contrast-dark",
        family_id: "high-contrast",
        name: "High Contrast Dark",
        description: "White on black, every edge drawn.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#000000"),
        shell: hex!("#000000"),
        raised: hex!("#1f1f1f"),
        card: hex!("#000000"),
        text: hex!("#ffffff"),
        muted: hex!("#d4d4d4"),
        faint: hex!("#b4b4b4"),
        accent: hex!("#f38518"),
        danger: hex!("#f48771"),
        warning: hex!("#cca700"),
        success: hex!("#89d185"),
        terminal_background: hex!("#000000"),
        ansi: [
            hex!("#000000"),
            hex!("#cd0000"),
            hex!("#00cd00"),
            hex!("#cdcd00"),
            hex!("#3b8eea"),
            hex!("#cd00cd"),
            hex!("#00cdcd"),
            hex!("#e5e5e5"),
            hex!("#7f7f7f"),
            hex!("#ff0000"),
            hex!("#00ff00"),
            hex!("#ffff00"),
            hex!("#5c5cff"),
            hex!("#ff00ff"),
            hex!("#00ffff"),
            hex!("#ffffff"),
        ],
        syntax: [
            hex!("#7ca668"),
            hex!("#569cd6"),
            hex!("#ce9178"),
            hex!("#b5cea8"),
            hex!("#4ec9b0"),
            hex!("#dcdcaa"),
            hex!("#9cdcfe"),
            hex!("#9cdcfe"),
            hex!("#ffffff"),
            hex!("#569cd6"),
            hex!("#9cdcfe"),
            hex!("#f44747"),
        ],
        source: source(
            "high-contrast-dark",
            "native",
            "https://github.com/aislopware/slopty",
            "d138049",
            "MIT",
        ),
    }))
}

// After VS Code's Light High Contrast (MIT): black on white, deep blue edges and focus.
fn high_contrast_light() -> ThemeVariant {
    high_contrast(variant(Seeds {
        id: "high-contrast-light",
        family_id: "high-contrast",
        name: "High Contrast Light",
        description: "Black on white, every edge drawn.",
        appearance: Appearance::Light,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#ffffff"),
        shell: hex!("#ffffff"),
        raised: hex!("#e8e8e8"),
        card: hex!("#ffffff"),
        text: hex!("#000000"),
        muted: hex!("#333333"),
        faint: hex!("#515151"),
        accent: hex!("#0f4a85"),
        danger: hex!("#b5200d"),
        warning: hex!("#895503"),
        success: hex!("#374e06"),
        terminal_background: hex!("#ffffff"),
        ansi: [
            hex!("#292929"),
            hex!("#cd3131"),
            hex!("#136c13"),
            hex!("#7a7a00"),
            hex!("#0451a5"),
            hex!("#bc05bc"),
            hex!("#0b6b85"),
            hex!("#555555"),
            hex!("#666666"),
            hex!("#cd3131"),
            hex!("#0a7a0a"),
            hex!("#6f7200"),
            hex!("#0451a5"),
            hex!("#bc05bc"),
            hex!("#0b6b85"),
            hex!("#1f1f1f"),
        ],
        syntax: [
            hex!("#515151"),
            hex!("#0f4a85"),
            hex!("#a31515"),
            hex!("#096d48"),
            hex!("#185e73"),
            hex!("#5e2cbc"),
            hex!("#001080"),
            hex!("#001080"),
            hex!("#000000"),
            hex!("#0f4a85"),
            hex!("#264f78"),
            hex!("#b5200d"),
        ],
        source: source(
            "high-contrast-light",
            "native",
            "https://github.com/aislopware/slopty",
            "d138049",
            "MIT",
        ),
    }))
}

fn tokyo_dark() -> ThemeVariant {
    variant(Seeds {
        id: "tokyo-night",
        family_id: "tokyo-night",
        name: "Tokyo Night",
        description: "Neon blues over a night city.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#1a1b26"),
        shell: hex!("#16161e"),
        raised: hex!("#292e42"),
        card: hex!("#1f2335"),
        text: hex!("#c0caf5"),
        muted: hex!("#9aa5ce"),
        faint: hex!("#7f87ad"),
        accent: hex!("#7aa2f7"),
        danger: hex!("#f7768e"),
        warning: hex!("#e0af68"),
        success: hex!("#9ece6a"),
        terminal_background: hex!("#1a1b26"),
        ansi: [
            hex!("#15161e"),
            hex!("#f7768e"),
            hex!("#9ece6a"),
            hex!("#e0af68"),
            hex!("#7aa2f7"),
            hex!("#bb9af7"),
            hex!("#7dcfff"),
            hex!("#a9b1d6"),
            hex!("#414868"),
            hex!("#f7768e"),
            hex!("#9ece6a"),
            hex!("#e0af68"),
            hex!("#7aa2f7"),
            hex!("#bb9af7"),
            hex!("#7dcfff"),
            hex!("#c0caf5"),
        ],
        syntax: [
            hex!("#565f89"),
            hex!("#bb9af7"),
            hex!("#9ece6a"),
            hex!("#ff9e64"),
            hex!("#2ac3de"),
            hex!("#7aa2f7"),
            hex!("#73daca"),
            hex!("#c0caf5"),
            hex!("#9abdf5"),
            hex!("#f7768e"),
            hex!("#e0af68"),
            hex!("#f7768e"),
        ],
        source: source(
            "tokyo-night",
            "vscode",
            "https://github.com/tokyo-night/tokyo-night-vscode-theme",
            "7c0f11eaef322f293621ca7befe462214b7ea468",
            "MIT",
        ),
    })
}

fn catppuccin_mocha() -> ThemeVariant {
    variant(Seeds {
        id: "catppuccin-mocha",
        family_id: "catppuccin",
        name: "Catppuccin Mocha",
        description: "Soft pastels on a warm dark.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#1e1e2e"),
        shell: hex!("#181825"),
        raised: hex!("#313244"),
        card: hex!("#242436"),
        text: hex!("#cdd6f4"),
        muted: hex!("#a6adc8"),
        faint: hex!("#9399b2"),
        accent: hex!("#cba6f7"),
        danger: hex!("#f38ba8"),
        warning: hex!("#f9e2af"),
        success: hex!("#a6e3a1"),
        terminal_background: hex!("#1e1e2e"),
        ansi: [
            hex!("#45475a"),
            hex!("#f38ba8"),
            hex!("#a6e3a1"),
            hex!("#f9e2af"),
            hex!("#89b4fa"),
            hex!("#f5c2e7"),
            hex!("#94e2d5"),
            hex!("#bac2de"),
            hex!("#585b70"),
            hex!("#f38ba8"),
            hex!("#a6e3a1"),
            hex!("#f9e2af"),
            hex!("#89b4fa"),
            hex!("#f5c2e7"),
            hex!("#94e2d5"),
            hex!("#a6adc8"),
        ],
        syntax: [
            hex!("#6c7086"),
            hex!("#cba6f7"),
            hex!("#a6e3a1"),
            hex!("#fab387"),
            hex!("#f9e2af"),
            hex!("#89b4fa"),
            hex!("#94e2d5"),
            hex!("#cdd6f4"),
            hex!("#bac2de"),
            hex!("#f38ba8"),
            hex!("#f5c2e7"),
            hex!("#f38ba8"),
        ],
        source: source(
            "catppuccin-mocha",
            "vscode",
            "https://github.com/catppuccin/vscode",
            "befc9e6fc41980f4241408f7049755d47c06ff45",
            "MIT",
        ),
    })
}

fn dracula() -> ThemeVariant {
    variant(Seeds {
        id: "dracula",
        family_id: "dracula",
        name: "Dracula",
        description: "Purple and pink, after dark.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#282a36"),
        shell: hex!("#21222c"),
        raised: hex!("#44475a"),
        card: hex!("#2d303e"),
        text: hex!("#f8f8f2"),
        muted: hex!("#c5c8c6"),
        faint: hex!("#9a9cab"),
        accent: hex!("#bd93f9"),
        danger: hex!("#ff5555"),
        warning: hex!("#f1fa8c"),
        success: hex!("#50fa7b"),
        terminal_background: hex!("#282a36"),
        ansi: [
            hex!("#21222c"),
            hex!("#ff5555"),
            hex!("#50fa7b"),
            hex!("#f1fa8c"),
            hex!("#bd93f9"),
            hex!("#ff79c6"),
            hex!("#8be9fd"),
            hex!("#f8f8f2"),
            hex!("#6272a4"),
            hex!("#ff6e6e"),
            hex!("#69ff94"),
            hex!("#ffffa5"),
            hex!("#d6acff"),
            hex!("#ff92df"),
            hex!("#a4ffff"),
            hex!("#ffffff"),
        ],
        syntax: [
            hex!("#6272a4"),
            hex!("#ff79c6"),
            hex!("#f1fa8c"),
            hex!("#bd93f9"),
            hex!("#8be9fd"),
            hex!("#50fa7b"),
            hex!("#8be9fd"),
            hex!("#f8f8f2"),
            hex!("#f8f8f2"),
            hex!("#ff79c6"),
            hex!("#50fa7b"),
            hex!("#ff5555"),
        ],
        source: source(
            "dracula",
            "vscode",
            "https://github.com/dracula/visual-studio-code",
            "1b9ecf4d7e0c8cc2e2e890a7a41ad1db5fff1e6c",
            "MIT",
        ),
    })
}

fn rose_pine_dawn() -> ThemeVariant {
    variant(Seeds {
        id: "rose-pine-dawn",
        family_id: "rose-pine",
        name: "Rosé Pine Dawn",
        description: "Muted rose on morning paper.",
        appearance: Appearance::Light,
        treatment: SurfaceTreatment::Opaque,
        background: hex!("#faf4ed"),
        shell: hex!("#f2e9e1"),
        raised: hex!("#dfdad9"),
        card: hex!("#fffaf3"),
        text: hex!("#575279"),
        muted: hex!("#797593"),
        faint: hex!("#9893a5"),
        accent: hex!("#907aa9"),
        danger: hex!("#b4637a"),
        warning: hex!("#ea9d34"),
        success: hex!("#56949f"),
        terminal_background: hex!("#faf4ed"),
        ansi: [
            hex!("#f2e9e1"),
            hex!("#b4637a"),
            hex!("#286983"),
            hex!("#ea9d34"),
            hex!("#56949f"),
            hex!("#907aa9"),
            hex!("#d7827e"),
            hex!("#575279"),
            hex!("#797593"),
            hex!("#b4637a"),
            hex!("#286983"),
            hex!("#ea9d34"),
            hex!("#56949f"),
            hex!("#907aa9"),
            hex!("#d7827e"),
            hex!("#575279"),
        ],
        syntax: [
            hex!("#9893a5"),
            hex!("#286983"),
            hex!("#ea9d34"),
            hex!("#d7827e"),
            hex!("#56949f"),
            hex!("#b4637a"),
            hex!("#56949f"),
            hex!("#d7827e"),
            hex!("#797593"),
            hex!("#56949f"),
            hex!("#907aa9"),
            hex!("#b4637a"),
        ],
        source: source(
            "rose-pine-dawn",
            "vscode",
            "https://github.com/rose-pine/vscode",
            "d8f5ebe8e096fa833e997c07eb7685ee1677a4ba",
            "MIT",
        ),
    })
}
