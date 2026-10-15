// SPDX-License-Identifier: LGPL-3.0-only
//! Widget signals honour `block_signals`.
//!
//! Qt reference: `QAbstractButton::click` emits `clicked` through `QObject::doActivate`, which checks
//! the button's own `blockSig` flag (qobject.cpp:4273). A blocked button emits nothing; unblocking
//! restores emission.

use ferrokit_core::object::QObject;
use ferrokit_widgets::button::Button;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn button_clicked_honours_block_signals() {
    let mut button = Button::new("ok");
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    button.clicked.connect(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
    });

    button.block_signals(true);
    button.clicked.emit(&());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "blocked Button emitted clicked"
    );

    button.block_signals(false);
    button.clicked.emit(&());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "unblocked Button did not emit clicked"
    );
}
