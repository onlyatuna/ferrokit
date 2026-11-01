// SPDX-License-Identifier: LGPL-3.0-only
//! A focus widget can keep Tab/Backtab for itself instead of letting them move the focus
//! (`QWidget::event` is overridable in Qt). `KeySequenceEdit` keeps them unless they are among its
//! finishing key combinations, which default to Tab and Backtab (`qkeysequenceedit.cpp`).

#[path = "support/probe_widget.rs"]
mod probe_widget;

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::focus::FocusPolicy;
use ferrokit_widgets::hit_test::EventTreeDispatcher;
use ferrokit_widgets::key_sequence_edit::{KeyCombination, KeySequenceEdit, key_modifier, qt_key};
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use probe_widget::{Log, ProbeWidget};
use std::cell::RefCell;
use std::rc::Rc;

const KEY_TAB: u32 = 0x0100_0001;
const KEY_BACKTAB: u32 = 0x0100_0002;
const MOD_CTRL: u32 = 0x0400_0000;

struct Fixture {
    root: WidgetRef,
    edit: WidgetRef,
    other_log: Log,
    dispatcher: EventTreeDispatcher,
}

fn fixture(finishing: Option<Vec<KeyCombination>>) -> Fixture {
    let mut edit = KeySequenceEdit::new();
    if let Some(finishing) = finishing {
        edit.set_finishing_key_combinations(finishing);
    }
    let edit: WidgetRef = Rc::new(RefCell::new(Box::new(edit)));
    let other_log = Log::default();
    let other = ProbeWidget::new(&other_log)
        .with_focus(FocusPolicy::StrongFocus)
        .into_ref();
    let root: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 200),
    ))));
    for child in [&edit, &other] {
        child.borrow().set_parent_widget(Some(Rc::downgrade(&root)));
        root.borrow_mut().add_child(Rc::clone(child));
    }
    let mut dispatcher = EventTreeDispatcher::new();
    let id = edit.borrow().id();
    dispatcher
        .focus_manager_mut()
        .set_focus(&root, Some(id), FocusReason::Tab);
    Fixture {
        root,
        edit,
        other_log,
        dispatcher,
    }
}

impl Fixture {
    fn press(&mut self, key: u32, modifiers: u32) {
        let mut ev = Event::new_spontaneous(EventKind::KeyPress {
            key,
            modifiers,
            is_repeat: false,
        });
        self.dispatcher.dispatch_event(&self.root, &mut ev);
    }

    fn focus_is_edit(&self) -> bool {
        self.dispatcher.focus_manager().focused_widget_id() == Some(self.edit.borrow().id())
    }

    fn recorded(&self) -> String {
        let w = self.edit.borrow();
        w.as_qobject_any()
            .and_then(|a| a.downcast_ref::<KeySequenceEdit>())
            .map(|e| e.key_sequence().to_portable_string())
            .unwrap()
    }
}

#[test]
fn default_finishing_combinations_are_tab_and_backtab() {
    let edit = KeySequenceEdit::new();
    assert_eq!(
        edit.finishing_key_combinations(),
        [
            KeyCombination::new(qt_key::TAB, 0),
            KeyCombination::new(qt_key::BACKTAB, 0)
        ]
    );
}

#[test]
fn tab_and_backtab_move_the_focus_off_a_key_sequence_edit_by_default() {
    let mut f = fixture(None);
    f.press(KEY_TAB, 0);
    assert!(
        !f.focus_is_edit(),
        "Tab is a finishing key, so it moves the focus"
    );
    assert_eq!(
        f.other_log.borrow().first().map(String::as_str),
        Some("focus_in")
    );

    let mut f = fixture(None);
    f.press(KEY_BACKTAB, 0);
    assert!(!f.focus_is_edit(), "Backtab moves the focus");

    let mut f = fixture(None);
    f.press(KEY_TAB, key_modifier::SHIFT);
    assert!(!f.focus_is_edit(), "Shift+Tab is Backtab");
}

#[test]
fn tab_is_recorded_when_it_is_not_a_finishing_combination() {
    let mut f = fixture(Some(Vec::new()));
    f.press(KEY_TAB, 0);
    assert!(f.focus_is_edit(), "the edit kept the focus");
    assert!(f.other_log.borrow().is_empty());
    assert_eq!(f.recorded(), "Tab");
}

#[test]
fn only_the_listed_tab_key_moves_the_focus() {
    let mut f = fixture(Some(vec![KeyCombination::new(qt_key::BACKTAB, 0)]));
    f.press(KEY_TAB, 0);
    assert!(f.focus_is_edit(), "Tab is not finishing, so it is recorded");
    assert_eq!(f.recorded(), "Tab");
    f.press(KEY_BACKTAB, 0);
    assert!(
        !f.focus_is_edit(),
        "Backtab is finishing, so it moves the focus"
    );
}

#[test]
fn ctrl_tab_still_does_not_move_the_focus() {
    let mut f = fixture(None);
    f.press(KEY_TAB, MOD_CTRL);
    assert!(f.focus_is_edit());
    assert!(f.other_log.borrow().is_empty());
}

#[test]
fn shift_tab_matches_only_the_default_backtab_entry() {
    let mut f = fixture(Some(vec![KeyCombination::new(qt_key::TAB, 0)]));
    f.press(KEY_TAB, key_modifier::SHIFT);
    assert!(f.focus_is_edit(), "Shift+Tab is not Tab: it is recorded");
    assert_eq!(f.recorded(), "Shift+Tab");

    let mut f = fixture(Some(vec![KeyCombination::new(
        qt_key::BACKTAB,
        key_modifier::SHIFT,
    )]));
    f.press(KEY_BACKTAB, key_modifier::SHIFT);
    assert!(!f.focus_is_edit(), "an exact Backtab+Shift entry matches");
    let mut f = fixture(Some(vec![KeyCombination::new(
        qt_key::BACKTAB,
        key_modifier::SHIFT,
    )]));
    f.press(KEY_BACKTAB, 0);
    assert!(f.focus_is_edit(), "Backtab alone is not that entry");
}
