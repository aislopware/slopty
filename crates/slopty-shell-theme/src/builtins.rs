// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::{
    AccentRoles, Appearance, Color, SurfaceTreatment, TerminalPalette, ThemeColors, ThemeFamily,
    ThemeRegistry, ThemeSource, ThemeVariant,
};

// A curated few (Amp's lesson): the house pair, a true black, a high-contrast pair, and
// four strong colourful ports, each with one line of voice (`docs/decisions/ui.md`).
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

fn family(id: &str, name: &str, variants: Vec<ThemeVariant>) -> ThemeFamily {
    ThemeFamily {
        id: id.into(),
        name: name.into(),
        variants,
    }
}

struct Seeds<'a> {
    id: &'a str,
    family_id: &'a str,
    name: &'a str,
    description: &'a str,
    appearance: Appearance,
    treatment: SurfaceTreatment,
    background: &'a str,
    shell: &'a str,
    raised: &'a str,
    card: &'a str,
    text: &'a str,
    muted: &'a str,
    faint: &'a str,
    accent: &'a str,
    danger: &'a str,
    warning: &'a str,
    success: &'a str,
    terminal_background: &'a str,
    ansi: [&'a str; 16],
    syntax: [&'a str; 12],
    source: ThemeSource,
}

fn variant(seed: Seeds<'_>) -> ThemeVariant {
    let background = c(seed.background);
    let shell = c(seed.shell);
    let raised = c(seed.raised);
    let card = c(seed.card);
    let text = c(seed.text);
    let muted = c(seed.muted).ensure_contrast(background, 4.5);
    let faint = c(seed.faint);
    let danger = c(seed.danger);
    let warning = c(seed.warning);
    let success = c(seed.success);
    let accent = AccentRoles::derive(c(seed.accent), seed.appearance, background);
    let dark = seed.appearance.is_dark();
    let border_tone = if dark { Color::WHITE } else { Color::BLACK };
    let solid = if dark {
        Color::rgb(235, 235, 239)
    } else {
        Color::rgb(35, 35, 40)
    };
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
    let terminal_background = c(seed.terminal_background);
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
            ansi: seed.ansi.map(c),
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
    let encoded = serde_json::to_vec(&variant).expect("built-in theme serializes");
    variant.source.asset_hash = format!("sha256:{}", data_encoding::HEXLOWER.encode(&Sha256::digest(encoded)));
}

fn syntax(colors: [&str; 12]) -> BTreeMap<String, Color> {
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
    ] = colors.map(c);
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

fn c(value: &str) -> Color {
    value.parse().expect("built-in theme colors are valid")
}

const ANSI_DARK: [&str; 16] = [
    "#242424", "#f87171", "#4ade80", "#facc15", "#60a5fa", "#c084fc", "#22d3ee", "#d4d4d8",
    "#52525b", "#fca5a5", "#86efac", "#fde047", "#93c5fd", "#d8b4fe", "#67e8f9", "#fafafa",
];

const ANSI_LIGHT: [&str; 16] = [
    "#1f1f1f", "#dc2626", "#16a34a", "#b45309", "#2563eb", "#9333ea", "#0e7490", "#3f3f46",
    "#71717a", "#b91c1c", "#15803d", "#92400e", "#1d4ed8", "#7e22ce", "#155e75", "#18181b",
];

fn slopty_dark() -> ThemeVariant {
    variant(Seeds {
        id: "slopty-dark",
        family_id: "slopty",
        name: "Slopty Dark",
        description: "Soft greys under a violet accent.",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Frosted,
        background: "#060606",
        shell: "#0d0d0d",
        raised: "#343438",
        card: "#0e0e0e",
        text: "#e8e8ea",
        muted: "#a9a9ae",
        faint: "#85858a",
        accent: "#8b7cf6",
        danger: "#f87171",
        warning: "#facc15",
        success: "#34d399",
        terminal_background: "#090909",
        ansi: ANSI_DARK,
        syntax: [
            "#92929a", "#8b7cf6", "#34d399", "#facc15", "#c084fc", "#60a5fa", "#f472b6", "#e8e8ea",
            "#a1a1aa", "#f472b6", "#22d3ee", "#f87171",
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
        background: "#ffffff",
        shell: "#f3f3f5",
        raised: "#ededf0",
        card: "#ffffff",
        text: "#303035",
        muted: "#62626a",
        faint: "#797981",
        accent: "#5b43e8",
        danger: "#dc2626",
        warning: "#a16207",
        success: "#15803d",
        terminal_background: "#fafafa",
        ansi: ANSI_LIGHT,
        syntax: [
            "#6b7280", "#5b43e8", "#15803d", "#a16207", "#7e22ce", "#2563eb", "#be185d", "#303035",
            "#52525b", "#be185d", "#0e7490", "#b91c1c",
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
        background: "#000000",
        shell: "#000000",
        raised: "#2a2a2e",
        card: "#050505",
        text: "#ededf0",
        muted: "#a9a9ae",
        faint: "#85858a",
        accent: "#8b7cf6",
        danger: "#f87171",
        warning: "#facc15",
        success: "#34d399",
        terminal_background: "#000000",
        ansi: ANSI_DARK,
        syntax: [
            "#92929a", "#8b7cf6", "#34d399", "#facc15", "#c084fc", "#60a5fa", "#f472b6", "#ededf0",
            "#a1a1aa", "#f472b6", "#22d3ee", "#f87171",
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
        background: "#000000",
        shell: "#000000",
        raised: "#1f1f1f",
        card: "#000000",
        text: "#ffffff",
        muted: "#d4d4d4",
        faint: "#b4b4b4",
        accent: "#f38518",
        danger: "#f48771",
        warning: "#cca700",
        success: "#89d185",
        terminal_background: "#000000",
        ansi: [
            "#000000", "#cd0000", "#00cd00", "#cdcd00", "#3b8eea", "#cd00cd", "#00cdcd", "#e5e5e5",
            "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
        ],
        syntax: [
            "#7ca668", "#569cd6", "#ce9178", "#b5cea8", "#4ec9b0", "#dcdcaa", "#9cdcfe", "#9cdcfe",
            "#ffffff", "#569cd6", "#9cdcfe", "#f44747",
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
        background: "#ffffff",
        shell: "#ffffff",
        raised: "#e8e8e8",
        card: "#ffffff",
        text: "#000000",
        muted: "#333333",
        faint: "#515151",
        accent: "#0f4a85",
        danger: "#b5200d",
        warning: "#895503",
        success: "#374e06",
        terminal_background: "#ffffff",
        ansi: [
            "#292929", "#cd3131", "#136c13", "#7a7a00", "#0451a5", "#bc05bc", "#0b6b85", "#555555",
            "#666666", "#cd3131", "#0a7a0a", "#6f7200", "#0451a5", "#bc05bc", "#0b6b85", "#1f1f1f",
        ],
        syntax: [
            "#515151", "#0f4a85", "#a31515", "#096d48", "#185e73", "#5e2cbc", "#001080", "#001080",
            "#000000", "#0f4a85", "#264f78", "#b5200d",
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
        background: "#1a1b26",
        shell: "#16161e",
        raised: "#292e42",
        card: "#1f2335",
        text: "#c0caf5",
        muted: "#9aa5ce",
        faint: "#7f87ad",
        accent: "#7aa2f7",
        danger: "#f7768e",
        warning: "#e0af68",
        success: "#9ece6a",
        terminal_background: "#1a1b26",
        ansi: [
            "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
            "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
        ],
        syntax: [
            "#565f89", "#bb9af7", "#9ece6a", "#ff9e64", "#2ac3de", "#7aa2f7", "#73daca", "#c0caf5",
            "#9abdf5", "#f7768e", "#e0af68", "#f7768e",
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
        background: "#1e1e2e",
        shell: "#181825",
        raised: "#313244",
        card: "#242436",
        text: "#cdd6f4",
        muted: "#a6adc8",
        faint: "#9399b2",
        accent: "#cba6f7",
        danger: "#f38ba8",
        warning: "#f9e2af",
        success: "#a6e3a1",
        terminal_background: "#1e1e2e",
        ansi: [
            "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
            "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
        ],
        syntax: [
            "#6c7086", "#cba6f7", "#a6e3a1", "#fab387", "#f9e2af", "#89b4fa", "#94e2d5", "#cdd6f4",
            "#bac2de", "#f38ba8", "#f5c2e7", "#f38ba8",
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
        background: "#282a36",
        shell: "#21222c",
        raised: "#44475a",
        card: "#2d303e",
        text: "#f8f8f2",
        muted: "#c5c8c6",
        faint: "#9a9cab",
        accent: "#bd93f9",
        danger: "#ff5555",
        warning: "#f1fa8c",
        success: "#50fa7b",
        terminal_background: "#282a36",
        ansi: [
            "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
            "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
        ],
        syntax: [
            "#6272a4", "#ff79c6", "#f1fa8c", "#bd93f9", "#8be9fd", "#50fa7b", "#8be9fd", "#f8f8f2",
            "#f8f8f2", "#ff79c6", "#50fa7b", "#ff5555",
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
        background: "#faf4ed",
        shell: "#f2e9e1",
        raised: "#dfdad9",
        card: "#fffaf3",
        text: "#575279",
        muted: "#797593",
        faint: "#9893a5",
        accent: "#907aa9",
        danger: "#b4637a",
        warning: "#ea9d34",
        success: "#56949f",
        terminal_background: "#faf4ed",
        ansi: [
            "#f2e9e1", "#b4637a", "#286983", "#ea9d34", "#56949f", "#907aa9", "#d7827e", "#575279",
            "#797593", "#b4637a", "#286983", "#ea9d34", "#56949f", "#907aa9", "#d7827e", "#575279",
        ],
        syntax: [
            "#9893a5", "#286983", "#ea9d34", "#d7827e", "#56949f", "#b4637a", "#56949f", "#d7827e",
            "#797593", "#56949f", "#907aa9", "#b4637a",
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
