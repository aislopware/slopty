// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! The style vocabulary a visual may use.
//!
//! Every value names something in the active Slopty theme (a color token, a step on the
//! transcript's type scale), so a script can't paint a background, pick a font, or hard-code a
//! color: the renderer resolves tokens against whatever theme, surface (glass or
//! opaque), appearance, and text size the reader has.

use crate::VisualError;

/// A theme color a visual may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    /// Primary text.
    Text,
    /// Labels, axes, captions.
    Muted,
    /// Tertiary notes.
    Faint,
    /// The one thing to notice; also the first chart series.
    Accent,
    /// Text drawn on an accent mark.
    OnAccent,
    /// Good news: a passing check, a gain.
    Success,
    /// Something to watch.
    Warning,
    /// A failure or a loss.
    Danger,
    /// Hairlines: rules, gridlines, axes.
    Border,
    /// Categorical series colors, `chart1` … `chart6` (0-based here).
    Chart(u8),
    /// A physical color (`0xRRGGBB`): only 3D scenes take these, for the
    /// materials of real objects (wood, foliage, a model's own paint).
    Rgb(u32),
}

/// How many categorical series colors the theme has.
pub const CHART_SERIES: u8 = 6;

/// Every token name a script may write, for error messages and the agent
/// guidance.
pub const TOKEN_NAMES: [&str; 15] = [
    "text",
    "muted",
    "faint",
    "accent",
    "on-accent",
    "success",
    "warning",
    "danger",
    "border",
    "chart1",
    "chart2",
    "chart3",
    "chart4",
    "chart5",
    "chart6",
];

impl Token {
    /// The token a script names (one of [`TOKEN_NAMES`]), or an error listing them.
    pub fn parse(name: &str) -> Result<Self, VisualError> {
        Ok(match name {
            "text" => Self::Text,
            "muted" => Self::Muted,
            "faint" => Self::Faint,
            "accent" => Self::Accent,
            "on-accent" => Self::OnAccent,
            "success" => Self::Success,
            "warning" => Self::Warning,
            "danger" => Self::Danger,
            "border" => Self::Border,
            other => match other.strip_prefix("chart").and_then(|n| n.parse::<u8>().ok()) {
                Some(n @ 1..=CHART_SERIES) => Self::Chart(n.saturating_sub(1)),
                _ => {
                    return Err(VisualError::script(format!(
                        "unknown color '{other}': colors are theme tokens ({}), so a visual \
                         reads on glass and opaque, light and dark",
                        TOKEN_NAMES.join(", ")
                    )));
                }
            },
        })
    }

    /// The `index`th categorical series color (0-based, cycling).
    pub fn series(index: usize) -> Self {
        // The remainder is below `CHART_SERIES`, so it fits a `u8`.
        let slot = index.checked_rem(usize::from(CHART_SERIES)).map(u8::try_from);
        Self::Chart(slot.and_then(Result::ok).unwrap_or_default())
    }
}

/// A resolved-at-paint color: a token, optionally mixed toward a second
/// token, at an opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// The base color.
    pub token: Token,
    /// A second token mixed in, and how much of it (0 none, 1 all).
    pub mix: Option<(Token, f32)>,
    /// Opacity, 0 to 1.
    pub alpha: f32,
}

impl Color {
    /// `token` as it is, opaque.
    pub const fn token(token: Token) -> Self {
        Self { token, mix: None, alpha: 1.0 }
    }

    /// This color with its opacity scaled by `alpha` (clamped to 0..=1).
    #[must_use]
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self { alpha: self.alpha * alpha.clamp(0.0, 1.0), ..self }
    }
}

/// A step on the transcript's type scale. Sizes are design pixels at the
/// 16px interface base; the renderer scales them with the reader's text size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextSize {
    /// `xs`: 11px on 16px lines.
    Xs,
    /// `sm`: 12px on 18px lines.
    Sm,
    /// `body`: 14px on 22px lines, the transcript's own.
    #[default]
    Body,
    /// `lg`: 15px on 22px lines.
    Lg,
    /// `xl`: 16px on 24px lines.
    Xl,
    /// `2xl`: 19px on 27px lines.
    Xxl,
}

impl TextSize {
    /// The step a script names (`xs`, `sm`, `body`, `lg`, `xl`, `2xl`), or an error.
    pub fn parse(name: &str) -> Result<Self, VisualError> {
        Ok(match name {
            "xs" => Self::Xs,
            "sm" => Self::Sm,
            "body" | "md" | "base" => Self::Body,
            "lg" => Self::Lg,
            "xl" => Self::Xl,
            "2xl" => Self::Xxl,
            other => {
                return Err(VisualError::script(format!(
                    "unknown text size '{other}': use xs, sm, body, lg, xl, or 2xl (the \
                     transcript's type scale)"
                )));
            }
        })
    }

    /// (font size, line height) in design pixels at the 16px base, the same
    /// steps the transcript's markdown uses.
    pub const fn metrics(self) -> (f32, f32) {
        match self {
            Self::Xs => (11.0, 16.0),
            Self::Sm => (12.0, 18.0),
            Self::Body => (14.0, 22.0),
            Self::Lg => (15.0, 22.0),
            Self::Xl => (16.0, 24.0),
            Self::Xxl => (19.0, 27.0),
        }
    }
}

/// Weights the interface font carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weight {
    /// 400.
    #[default]
    Regular,
    /// 500.
    Medium,
    /// 600.
    Semibold,
    /// 700.
    Bold,
}

impl Weight {
    /// The weight a CSS number (400 regular … 700 bold) rounds to.
    pub const fn from_integer(weight: i64) -> Self {
        match weight {
            ..=449 => Self::Regular,
            450..=549 => Self::Medium,
            550..=649 => Self::Semibold,
            _ => Self::Bold,
        }
    }

    /// [`Self::from_integer`] for a fractional number, its fraction dropped;
    /// NaN reads as regular.
    pub const fn from_number(weight: f64) -> Self {
        if weight.is_nan() || weight < 450.0 {
            Self::Regular
        } else if weight < 550.0 {
            Self::Medium
        } else if weight < 650.0 {
            Self::Semibold
        } else {
            Self::Bold
        }
    }

    /// The weight a script names (`regular`, `medium`, `semibold`, `bold`), or an error.
    pub fn parse(name: &str) -> Result<Self, VisualError> {
        Ok(match name {
            "regular" | "normal" => Self::Regular,
            "medium" => Self::Medium,
            "semibold" => Self::Semibold,
            "bold" => Self::Bold,
            other => {
                return Err(VisualError::script(format!(
                    "unknown weight '{other}': use 400-700 or regular, medium, semibold, bold"
                )));
            }
        })
    }
}

/// Where children sit across a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// At the left, or the top.
    #[default]
    Start,
    /// In the middle.
    Center,
    /// At the right, or the bottom.
    End,
}

impl Align {
    /// The alignment a script names (`start`, `center`, `end`, or a side), or an error.
    pub fn parse(name: &str) -> Result<Self, VisualError> {
        Ok(match name {
            "start" | "left" | "top" => Self::Start,
            "center" | "middle" => Self::Center,
            "end" | "right" | "bottom" => Self::End,
            other => {
                return Err(VisualError::script(format!(
                    "unknown alignment '{other}': use start, center, or end"
                )));
            }
        })
    }
}

/// How children share a stack's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    /// Packed at the start.
    #[default]
    Start,
    /// Packed in the middle.
    Center,
    /// Packed at the end.
    End,
    /// Spread out, the space between them equal.
    Between,
}

impl Justify {
    /// The justification a script names (an [`Align`] or `between`), or an error.
    pub fn parse(name: &str) -> Result<Self, VisualError> {
        Ok(match name {
            "between" | "space-between" => Self::Between,
            other => match Align::parse(other)? {
                Align::Start => Self::Start,
                Align::Center => Self::Center,
                Align::End => Self::End,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_theme_tokens_only() {
        assert_eq!(Token::parse("chart1").unwrap(), Token::Chart(0));
        assert_eq!(Token::parse("chart6").unwrap(), Token::Chart(5));
        for bad in ["chart0", "chart7", "#ff0000", "red", "rgb(0,0,0)", "background"] {
            let err = Token::parse(bad).unwrap_err().to_string();
            assert!(err.contains("theme tokens"), "{bad}: {err}");
        }
        assert_eq!(Token::series(7), Token::Chart(1));
    }

    #[test]
    fn sizes_follow_the_transcript_scale() {
        assert_eq!(TextSize::parse("body").unwrap().metrics(), (14.0, 22.0));
        assert_eq!(TextSize::parse("2xl").unwrap().metrics(), (19.0, 27.0));
        TextSize::parse("40px").unwrap_err();
        assert_eq!(Weight::from_number(600.0), Weight::Semibold);
    }
}
