// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_widgets::{
    set_widget_enabled, AccessibleAction, AccessibleId, AccessibleNode, AccessibleRole,
    AccessibleState, AccessibleTree, Button, Widget, WidgetRef,
};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn widget_snapshot_uses_widget_semantics_and_focus_state() {
    let button = Button::new("Save");
    button.set_has_focus(true);
    let widget: WidgetRef = std::rc::Rc::new(RefCell::new(Box::new(button)));

    let tree = AccessibleTree::from_widget(&widget);
    let root = tree.root().unwrap();
    let node = tree.node(root).unwrap();

    assert_eq!(node.role, AccessibleRole::Button);
    assert_eq!(node.name, "Save");
    assert!(node.has_state(AccessibleState::Focused));
    assert!(node.supports_action(AccessibleAction::Invoke));
}

#[test]
fn accessible_invoke_activates_button_once_and_respects_enabled_state() {
    let button = Button::new("Run");
    let activations = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = activations.clone();
    let _connection = button.clicked.connect(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    });
    let widget: WidgetRef = std::rc::Rc::new(RefCell::new(Box::new(button)));

    assert!(widget
        .borrow_mut()
        .perform_accessible_action(AccessibleAction::Invoke));
    set_widget_enabled(&widget, false);
    assert!(!widget
        .borrow_mut()
        .perform_accessible_action(AccessibleAction::Invoke));
    assert_eq!(activations.load(Ordering::SeqCst), 1);
}

#[test]
fn accessible_tree_preserves_hierarchy_and_rejects_cycles() {
    let root = AccessibleId(1);
    let panel = AccessibleId(2);
    let button = AccessibleId(3);
    let mut tree = AccessibleTree::new();

    assert!(tree
        .set_root(AccessibleNode::new(
            root,
            AccessibleRole::Window,
            "Main window"
        ))
        .is_ok());
    assert!(tree
        .insert(
            panel,
            AccessibleNode::new(button, AccessibleRole::Button, "Save")
        )
        .is_err());
    assert!(tree
        .insert(
            root,
            AccessibleNode::new(panel, AccessibleRole::Group, "Editor")
        )
        .is_ok());

    let mut save = AccessibleNode::new(button, AccessibleRole::Button, "Save");
    save.actions.push(AccessibleAction::Invoke);
    save.states.push(AccessibleState::Focusable);
    assert!(tree.insert(panel, save).is_ok());

    assert_eq!(tree.root(), Some(root));
    assert_eq!(tree.parent(button), Some(panel));
    assert_eq!(tree.children(panel), &[button]);
    assert!(tree
        .node(button)
        .unwrap()
        .supports_action(AccessibleAction::Invoke));
    assert!(!tree.reparent(panel, button));
    assert!(tree.reparent(button, root));
    assert_eq!(tree.parent(button), Some(root));

    assert!(tree.remove(panel));
    assert!(tree.node(panel).is_none());
    assert!(tree.node(button).is_some());
    assert!(!tree.remove(root));
}

#[test]
fn accessible_tree_removal_deletes_subtrees() {
    let mut tree = AccessibleTree::new();
    tree.set_root(AccessibleNode::new(
        AccessibleId(10),
        AccessibleRole::Application,
        "Application",
    ))
    .unwrap();
    tree.insert(
        AccessibleId(10),
        AccessibleNode::new(AccessibleId(11), AccessibleRole::Dialog, "Preferences"),
    )
    .unwrap();
    tree.insert(
        AccessibleId(11),
        AccessibleNode::new(AccessibleId(12), AccessibleRole::CheckBox, "Enable feature"),
    )
    .unwrap();

    assert!(tree.remove(AccessibleId(11)));
    assert!(tree.node(AccessibleId(11)).is_none());
    assert!(tree.node(AccessibleId(12)).is_none());
    assert!(tree.children(AccessibleId(10)).is_empty());
}
