// SPDX-License-Identifier: LGPL-3.0-only
//! A `Quit` addressed to the application object is an event: application event filters see it,
//! and a consumed Quit must not end exec (qcoreapplication.cpp:1285-1290, 2054-2056).
use parking_lot::Mutex;

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{clear_application_event_filters, install_application_event_filter};
use ferrokit_core::object::{register_qobject, unregister_qobject, ObjectData, ObjectId, QObject};
use ferrokit_core::timer::Timer;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Application event filter that consumes every `Quit` (`QObject::eventFilter` returns true).
struct ConsumeQuit {
    data: ObjectData,
}

impl QObject for ConsumeQuit {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
        matches!(event.kind, EventKind::Quit { .. })
    }
}

#[test]
fn posted_quit_consumed_by_application_filter_does_not_end_exec() {
    let _guard = TEST_MUTEX.lock();
    CoreApplication::reset_for_test();
    let mut app = CoreApplication::new(vec!["quit_veto".to_string()]);
    let app_id = CoreApplication::instance_id().expect("application object");
    let mut filter = ConsumeQuit {
        data: ObjectData::new(ObjectId::next()),
    };
    let filter_id = filter.data.id;
    // SAFETY: the filter stays on this thread and is unregistered before it is dropped.
    unsafe { register_qobject(&mut filter) };
    install_application_event_filter(filter_id);
    Timer::single_shot(10, move || {
        CoreApplication::post_event(app_id, Event::new(EventKind::Quit { exit_code: 0 }));
    });
    Timer::single_shot(500, || CoreApplication::exit(7)); // watchdog
    let code = app.exec();
    clear_application_event_filters();
    unsafe { unregister_qobject(filter_id) };
    assert_eq!(
        code, 7,
        "an application filter that consumes a posted Quit must keep exec running"
    );
}

#[test]
fn quit_consumed_by_application_filter_does_not_end_exec() {
    let _guard = TEST_MUTEX.lock();
    CoreApplication::reset_for_test();
    let mut app = CoreApplication::new(vec!["quit_veto".to_string()]);
    let mut filter = ConsumeQuit {
        data: ObjectData::new(ObjectId::next()),
    };
    let filter_id = filter.data.id;
    // SAFETY: as above.
    unsafe { register_qobject(&mut filter) };
    install_application_event_filter(filter_id);
    // QCoreApplication::quit on the application thread is sendEvent (qcoreapplication.cpp:2153-2155).
    Timer::single_shot(10, || CoreApplication::quit());
    Timer::single_shot(500, || CoreApplication::exit(7)); // watchdog
    let code = app.exec();
    clear_application_event_filters();
    unsafe { unregister_qobject(filter_id) };
    assert_eq!(
        code, 7,
        "quit() is sendEvent(Quit) on the app thread: a filter that consumes it keeps exec running"
    );
}
