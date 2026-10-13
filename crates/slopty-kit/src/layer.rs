//! Where every floating layer stacks.

/// Where every floating layer stacks, bottom to top, painted through
/// `gpui::deferred(..).with_priority(layer.priority())`.
///
/// Named once, so a notice cannot fall under a dialog, nor a menu opened from a popover under
/// the popover. A panel that is part of the frame is drawn at the default priority, under all
/// of them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Layer {
    /// Anchored to what opened it and dismissed by a click away: the inbox, a menu, the hosts.
    Popover,
    /// Opened from a popover or a row inside one: a block's menu.
    Submenu,
    /// Modal over the window and its scrim: the palette, the picker, the settings.
    Dialog,
    /// A notice in the title bar or by its tile: laid out in place, painted over whatever else is
    /// up, so the inbox's "Undo" is pressed through an open popover's click-away.
    Toast,
}

impl Layer {
    /// Its paint priority for `gpui::deferred`: a higher one paints over a lower one.
    #[must_use]
    pub const fn priority(self) -> usize {
        match self {
            Self::Popover => 1,
            Self::Submenu => 2,
            Self::Dialog => 3,
            Self::Toast => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Layer;

    /// The layers stack in the one order: a popover, a menu from it, a dialog, a notice.
    #[test]
    fn the_layers_stack_popover_submenu_dialog_toast() {
        let order = [Layer::Popover, Layer::Submenu, Layer::Dialog, Layer::Toast];
        assert!(order.windows(2).all(|w| w[0].priority() < w[1].priority()), "{order:?}");
        assert!(Layer::Popover.priority() > 0, "over a panel drawn at the default");
    }
}
