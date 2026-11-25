//! Where desktop widgets are (pure): on the desktop layer, or summoned in
//! front of everything for a moment.

/// The layer the widgets are on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layer {
    /// Behind every app window, with the desktop (the usual place).
    #[default]
    Desktop,
    /// In front of everything (topmost) and focused, until sent back.
    Summoned,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The widgets' shortcut: summons them, or sends them back.
    Hotkey,
    /// Esc in a widget.
    Escape,
    /// The foreground went to a window that is not one of the widgets
    /// (a click outside them, Alt+Tab, another app's window).
    FocusLost,
}

/// What to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Bring every widget to the front, focus the last one used.
    Summon,
    /// Back to the desktop layer. `restore`: give the focus back to the
    /// window that had it before the summon (the user sent them back with
    /// a key; after a click elsewhere the focus is already where it goes).
    Dismiss { restore: bool },
    /// Esc on the desktop layer: done typing here, the focus leaves the
    /// widget (to the window that had it before, else the desktop).
    Release,
}

/// Pure: the layer after `ev` on `now`, and what to do.
pub fn next(now: Layer, ev: Event) -> (Layer, Action) {
    match (now, ev) {
        (Layer::Desktop, Event::Hotkey) => (Layer::Summoned, Action::Summon),
        (Layer::Summoned, Event::Hotkey) | (Layer::Summoned, Event::Escape) => (Layer::Desktop, Action::Dismiss { restore: true }),
        (Layer::Summoned, Event::FocusLost) => (Layer::Desktop, Action::Dismiss { restore: false }),
        (Layer::Desktop, Event::Escape) => (Layer::Desktop, Action::Release),
        (Layer::Desktop, Event::FocusLost) => (Layer::Desktop, Action::Nothing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortcut_summons_and_sends_back() {
        assert_eq!(next(Layer::Desktop, Event::Hotkey), (Layer::Summoned, Action::Summon));
        assert_eq!(next(Layer::Summoned, Event::Hotkey), (Layer::Desktop, Action::Dismiss { restore: true }));
    }

    #[test]
    fn esc_sends_back_and_gives_the_focus_back() {
        assert_eq!(next(Layer::Summoned, Event::Escape), (Layer::Desktop, Action::Dismiss { restore: true }));
        // On the desktop it only lets go of the keyboard.
        assert_eq!(next(Layer::Desktop, Event::Escape), (Layer::Desktop, Action::Release));
    }

    #[test]
    fn a_click_elsewhere_sends_back_without_stealing_the_focus() {
        assert_eq!(next(Layer::Summoned, Event::FocusLost), (Layer::Desktop, Action::Dismiss { restore: false }));
        // Editing on the desktop and clicking away changes nothing.
        assert_eq!(next(Layer::Desktop, Event::FocusLost), (Layer::Desktop, Action::Nothing));
    }

    #[test]
    fn a_round_trip_ends_where_it_started() {
        let mut l = Layer::default();
        for ev in [Event::Hotkey, Event::Escape, Event::Hotkey, Event::FocusLost, Event::Hotkey, Event::Hotkey] {
            l = next(l, ev).0;
        }
        assert_eq!(l, Layer::Desktop);
    }
}
