// SPDX-License-Identifier: LGPL-3.0-only
//! `QCoreApplication::exec` flushes DeferredDelete events when the loop returns
//! (qcoreapplication.cpp:1472-1473 and 1484-1489, `execCleanup`).

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::post_event_to_thread;
use ferrokit_core::object::{
    query_object_thread, register_owned_qobject, ObjectData, ObjectId, OwnedObject, QObject,
    ThreadId,
};

struct Victim {
    data: ObjectData,
}

impl QObject for Victim {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::MouseButtonPress { .. }) {
            // Requested from the last handler, then the loop is asked to quit.
            self.delete_later();
            CoreApplication::quit();
            true
        } else {
            false
        }
    }
}

#[test]
fn test_exec_flushes_deferred_delete_requested_by_last_handler() {
    let mut app = CoreApplication::new(vec!["test".to_string()]);
    // SAFETY: `victim` is a local on this thread that the test neither uses nor drops while
    // `app.exec()` runs; the only code touching the object then is its own `event` callback,
    // which asks for `delete_later`, the crate's deferred destruction, and does not touch the owner.
    let victim = unsafe {
        register_owned_qobject(Victim {
            data: ObjectData::new(ObjectId::next()),
        })
    };
    let wid = OwnedObject::object_id(&victim);

    let ev = Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 0,
        y: 0,
        button: 1,
    });
    post_event_to_thread(ThreadId::current(), wid, ev);

    app.exec();

    assert_eq!(
        query_object_thread(wid),
        None,
        "deleteLater requested by the last handler was not flushed when exec returned"
    );
}
