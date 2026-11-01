// SPDX-License-Identifier: LGPL-3.0-only

use std::ptr::NonNull;

use ferrokit_core::event::EventKind;
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    ObjectId, QObject, ThreadId, query_object_thread, register_qobject_ptr,
};
use ferrokit_widgets::menu::Menu;
use ferrokit_widgets::progress_bar::ProgressBar;
use ferrokit_widgets::widget::Widget;

#[test]
fn menu_update_posts_to_menu_affinity_thread_when_it_has_no_window() {
    let event_loop = EventLoop::new();
    let menu = Menu::new("thread target");
    let id = menu.id();
    let affinity = menu.object_data().thread_id();

    menu.update();

    assert_eq!(affinity, ThreadId::current());
    assert!(event_loop.queue().lock().unwrap().events().iter().any(
        |posted| posted.receiver == id && matches!(posted.event.kind, EventKind::UpdateRequest)
    ));
}

#[test]
fn progress_bar_drop_unregisters_on_its_registration_thread() {
    let mut bar = ProgressBar::new();
    let id = bar.id();
    assert_eq!(bar.object_data().thread_id(), ThreadId::current());
    // SAFETY: the bar remains unmoved until Drop unregisters it; it is dropped on this thread.
    unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut bar)) };

    drop(bar);

    assert!(ferrokit_core::object::with_object(id, |_| ()).is_none());
    assert_ne!(id, ObjectId(0));
    assert_eq!(query_object_thread(id), None);
}
