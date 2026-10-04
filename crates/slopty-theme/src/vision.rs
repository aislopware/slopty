//! The tones that tell states apart, seen as a colour-blind person sees them.
//!
//! Each pair the chrome sets side by side to mean opposite things (lines added and removed, a
//! live mark and a failure, waiting and failed) is drawn through Machado, Oliveira and
//! Fernandes's simulation of the three dichromacies at full severity (2009, the matrices in
//! linear sRGB), in both appearances, and measured apart in Oklab. Increase Contrast is not
//! held to it: its lift carries every tone toward the pole, so the tones meet there; the
//! signs and marks beside them carry the difference.
//! Each must stay as far apart as [`APART`]: past a just-noticeable step, far enough to be
//! told apart at a glance in a diff's gutter, not only side by side.

use super::{Contrast, Rgb, Theme, Variant};

/// The least Oklab distance two opposite tones keep under every simulation: about three
/// just-noticeable differences (0.02 each in Oklab).
const APART: f32 = 0.06;

/// Machado et al. (2009), severity 1.0, rows of linear sRGB.
const PROTANOPIA: [[f32; 3]; 3] = [
    [0.152_286, 1.052_583, -0.204_868],
    [0.114_503, 0.786_281, 0.099_216],
    [-0.003_882, -0.048_116, 1.051_998],
];
const DEUTERANOPIA: [[f32; 3]; 3] = [
    [0.367_322, 0.860_646, -0.227_968],
    [0.280_085, 0.672_501, 0.047_413],
    [-0.011_820, 0.042_940, 0.968_881],
];
const TRITANOPIA: [[f32; 3]; 3] = [
    [1.255_528, -0.076_749, -0.178_779],
    [-0.078_411, 0.930_809, 0.147_602],
    [0.004_733, 0.691_367, 0.303_900],
];

/// One sRGB channel, 0 to 255, as linear light.
fn linear(c: u8) -> f32 {
    let v = f32::from(c) / 255.0;
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// `c` seen through `m`, as linear sRGB clamped to the gamut.
fn seen(c: Rgb, m: &[[f32; 3]; 3]) -> [f32; 3] {
    let rgb = [linear(c.r), linear(c.g), linear(c.b)];
    m.map(|row| row.iter().zip(rgb).map(|(k, v)| k * v).sum::<f32>().clamp(0.0, 1.0))
}

/// Linear sRGB in Oklab (Björn Ottosson's matrices).
#[expect(clippy::many_single_char_names, reason = "Ottosson's own names: r g b in, l m s cones")]
fn oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = 0.051_445_99_f32.mul_add(b, 0.412_221_46_f32.mul_add(r, 0.536_332_55 * g)).cbrt();
    let m = 0.107_396_96_f32.mul_add(b, 0.211_903_5_f32.mul_add(r, 0.680_699_5 * g)).cbrt();
    let s = 0.629_978_7_f32.mul_add(b, 0.088_302_46_f32.mul_add(r, 0.281_718_84 * g)).cbrt();
    [
        (-0.004_072_047_f32).mul_add(s, 0.210_454_26_f32.mul_add(l, 0.793_617_8 * m)),
        0.450_593_7_f32.mul_add(s, 1.977_998_5_f32.mul_add(l, -2.428_592_2 * m)),
        (-0.808_675_77_f32).mul_add(s, 0.025_904_037_f32.mul_add(l, 0.782_771_77 * m)),
    ]
}

/// How far apart `a` and `b` look through `m`, in Oklab.
fn apart(a: Rgb, b: Rgb, m: &[[f32; 3]; 3]) -> f32 {
    let (a, b) = (oklab(seen(a, m)), oklab(seen(b, m)));
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

#[test]
fn opposite_states_stay_apart_for_every_dichromacy() {
    let sights =
        [("protanopia", PROTANOPIA), ("deuteranopia", DEUTERANOPIA), ("tritanopia", TRITANOPIA)];
    let mut short = Vec::new();
    for variant in [Variant::Dark, Variant::Light] {
        for contrast in [Contrast::Standard] {
            let mut theme = Theme::new(variant);
            theme.contrast = contrast;
            theme.derive_chrome();
            let s = theme.surfaces;
            let pairs = [
                ("added vs removed", s.success, s.error),
                ("live vs error", s.accent, s.error),
                ("warn vs error", s.warn, s.error),
            ];
            for (name, sight) in &sights {
                for (pair, a, b) in pairs {
                    let d = apart(a, b, sight);
                    if d < APART {
                        short.push(format!(
                            "{variant:?} {contrast:?} {name} {pair} ({a:?} {b:?}): {d:.3}"
                        ));
                    }
                }
            }
        }
    }
    assert!(short.is_empty(), "too close (want {APART}):\n{}", short.join("\n"));
}

/// The simulation holds what it should: a grey is seen as itself, and red and green, far
/// apart to a trichromat, come close for a protanope and a deuteranope.
#[test]
fn the_simulation_keeps_greys_and_merges_red_with_green() {
    let grey = Rgb { r: 128, g: 128, b: 128 };
    for m in [PROTANOPIA, DEUTERANOPIA, TRITANOPIA] {
        assert!(apart(grey, grey, &m) < f32::EPSILON);
        let [r, g, b] = seen(grey, &m);
        assert!((r - g).abs() < 0.01 && (g - b).abs() < 0.01, "a grey stays grey: {r} {g} {b}");
    }
    // Ely's pair: matplotlib's red and green.
    let (red, green) = (Rgb::hex(0xd6_2728), Rgb::hex(0x2c_a02c));
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    assert!(apart(red, green, &identity) > 0.30, "far apart to a trichromat");
    assert!(apart(red, green, &DEUTERANOPIA) < 0.05, "one colour to a deuteranope");
}

/// The agent's orange and the waiting amber sit near in hue, so they are held apart for
/// normal vision too: the agent's mark is a glyph and waiting a dot or a word, and shape tells
/// them apart for a dichromat, but at a glance in colour they must not drift into one brown.
#[test]
fn the_agent_stays_apart_from_waiting() {
    let normal = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for variant in [Variant::Dark, Variant::Light] {
        let s = Theme::new(variant).surfaces;
        let d = apart(s.agent, s.warn, &normal);
        assert!(
            d >= APART,
            "{variant:?}: the agent {:?} and waiting {:?}: {d:.3}",
            s.agent,
            s.warn
        );
    }
}
