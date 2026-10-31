// SPDX-License-Identifier: LGPL-3.0-only
//! Round-3 review: stacked pages, hide-time focus handover and a deferred disable.

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::Point;
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

type Log = Rc<RefCell<Vec<String>>>;

struct Probe {
    base: WidgetBase,
    name: &'static str,
    log: Log,
    /// Runs on every `Show` / `Hide`, with the widget borrowed.
    hook: RefCell<Option<Rc<dyn Fn()>>>,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        let tag = match event.kind {
            EventKind::Show => "Show",
            EventKind::Hide => "Hide",
            _ => return false,
        };
        self.log.borrow_mut().push(format!("{}:{tag}", self.name));
        let hook = self.hook.borrow().clone();
        if let Some(hook) = hook {
            hook();
        }
        false
    }
}

impl Widget for Probe {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {
        self.base.update();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn layout(&self) -> Option<&dyn Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }
    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }
    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }
    fn focus_in_event(&mut self, reason: FocusReason) {
        self.log
            .borrow_mut()
            .push(format!("{}:In:{reason:?}", self.name));
    }
    fn focus_out_event(&mut self, reason: FocusReason) {
        self.log
            .borrow_mut()
            .push(format!("{}:Out:{reason:?}", self.name));
    }
}

fn probe(name: &'static str, policy: FocusPolicy, log: &Log) -> WidgetRef {
    let base = WidgetBase::with_geometry(Rect::new(0, 0, 10, 10));
    base.set_focus_policy(policy);
    Rc::new(RefCell::new(Box::new(Probe {
        base,
        name,
        log: Rc::clone(log),
        hook: RefCell::new(None),
    })))
}

fn set_hook(w: &WidgetRef, f: impl Fn() + 'static) {
    let g = w.borrow();
    let p = g.as_any().downcast_ref::<Probe>().unwrap();
    *p.hook.borrow_mut() = Some(Rc::new(f));
}

fn stack() -> WidgetRef {
    Rc::new(RefCell::new(Box::new(StackedWidget::new())))
}

fn count(s: &WidgetRef) -> usize {
    s.borrow()
        .as_any()
        .downcast_ref::<StackedWidget>()
        .unwrap()
        .count()
}

fn current(s: &WidgetRef) -> usize {
    s.borrow()
        .as_any()
        .downcast_ref::<StackedWidget>()
        .unwrap()
        .current_index()
}

fn add(s: &WidgetRef, page: &WidgetRef) {
    StackedWidget::add_widget(s, page.clone());
    adopt_tree(s);
}

/// Qt `QStackedLayout::setGeometry` never shows or hides a page (qstackedlayout.cpp:453-468).
/// A page added behind the current one is hidden when it is added, so a later resize has nothing
/// to change, and the layout's borrow is not held while the pages run: a `Hide` handler can read
/// the stack.
#[test]
fn set_geometry_does_not_change_visibility_and_holds_no_layout_borrow() {
    let log = Log::default();
    let s = stack();
    let (p0, p1) = (
        probe("p0", FocusPolicy::NoFocus, &log),
        probe("p1", FocusPolicy::NoFocus, &log),
    );
    add(&s, &p0);
    add(&s, &p1);
    let reads = Rc::new(std::cell::Cell::new(0));
    let (s2, r2) = (s.clone(), reads.clone());
    set_hook(&p1, move || {
        r2.set(r2.get() + count(&s2) + current(&s2));
    });
    log.borrow_mut().clear();

    // The user hides the current page, and shows the other one explicitly.
    set_widget_visible(&p0, false);
    set_widget_visible(&p1, true);
    log.borrow_mut().clear();
    s.borrow().set_geometry(Rect::new(0, 0, 100, 80));

    assert!(
        !p0.borrow().is_visible(),
        "a resize must not show the current page"
    );
    assert!(
        p1.borrow().is_visible(),
        "a resize must not hide the other page"
    );
    assert!(log.borrow().is_empty(), "no Show/Hide: {:?}", log.borrow());
}

/// Inserting at or before the current page keeps the current page current; the inserted page is
/// hidden. Removing the page before the current one keeps it current, removing the current one
/// makes its neighbour current, and the removed page is hidden.
#[test]
fn insert_and_remove_adjust_the_current_index_and_hide_the_removed_page() {
    let log = Log::default();
    let mut l = StackedLayout::new();
    let pages: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|n| probe(n, FocusPolicy::NoFocus, &log))
        .collect();
    for p in &pages {
        l.add_widget(p.clone());
    }
    l.set_current_index(2);
    assert_eq!(l.current_index(), 2);
    assert!(pages[2].borrow().is_visible() && !pages[0].borrow().is_visible());

    let new = probe("n", FocusPolicy::NoFocus, &log);
    l.insert_widget(0, new.clone());
    assert_eq!(l.current_index(), 3, "the current page stays the same page");
    assert!(Rc::ptr_eq(&l.current_widget().unwrap(), &pages[2]));
    assert!(
        !new.borrow().is_visible(),
        "an inserted non-current page is hidden"
    );
    assert!(pages[2].borrow().is_visible());

    let removed = l.remove_widget(0).unwrap();
    assert!(Rc::ptr_eq(&removed, &new));
    assert_eq!(l.current_index(), 2);
    assert!(!removed.borrow().is_visible());

    // Removing the current (last) page makes the one before it current and shown.
    let removed = l.remove_widget(2).unwrap();
    assert!(Rc::ptr_eq(&removed, &pages[2]));
    assert!(!removed.borrow().is_visible(), "the removed page is hidden");
    assert_eq!(l.current_index(), 1);
    assert!(pages[1].borrow().is_visible());
    assert_eq!(l.count(), 2);
}

/// `StackedWidget` inserts and removes pages the same way, and announces a new current page.
#[test]
fn stacked_widget_insert_and_remove_follow_the_layout() {
    let log = Log::default();
    let s = stack();
    let pages: Vec<_> = ["a", "b"]
        .iter()
        .map(|n| probe(n, FocusPolicy::NoFocus, &log))
        .collect();
    for p in &pages {
        add(&s, p);
    }
    let new = probe("n", FocusPolicy::NoFocus, &log);
    StackedWidget::insert_widget(&s, 0, new.clone());
    assert_eq!(current(&s), 1);
    let removed = StackedWidget::remove_widget(&s, 1).unwrap();
    assert!(Rc::ptr_eq(&removed, &pages[0]));
    assert!(!removed.borrow().is_visible());
    assert_eq!(count(&s), 2);
    assert!(
        s.borrow()
            .children()
            .iter()
            .any(|c| Rc::ptr_eq(c, &removed)),
        "Qt keeps the parent"
    );
}

/// `QWidgetPrivate::hide_helper` moves the focus on when the focus widget, or an ancestor of it,
/// is hidden (qwidget.cpp:8276-8283).
fn focus_tree() -> (
    WidgetRef,
    WidgetRef,
    WidgetRef,
    WidgetRef,
    EventTreeDispatcher,
    Log,
) {
    let log = Log::default();
    let root = probe("root", FocusPolicy::NoFocus, &log);
    let a = probe("a", FocusPolicy::StrongFocus, &log);
    let b = probe("b", FocusPolicy::StrongFocus, &log);
    let c = probe("c", FocusPolicy::NoFocus, &log);
    c.borrow_mut().add_child(a.clone());
    root.borrow_mut().add_child(c.clone());
    root.borrow_mut().add_child(b.clone());
    adopt_tree(&root);
    let mut d = EventTreeDispatcher::new();
    let id = a.borrow().id();
    assert!(d
        .focus_manager_mut()
        .set_focus(&root, Some(id), FocusReason::Other));
    (root, a, b, c, d, log)
}

#[test]
fn hiding_the_focus_widget_moves_the_focus() {
    let (_root, a, b, _c, d, _log) = focus_tree();
    set_widget_visible(&a, false);
    assert_eq!(d.focus_manager().focused_widget_id(), Some(b.borrow().id()));
    assert!(!a.borrow().has_focus());
    assert!(b.borrow().has_focus());
}

#[test]
fn hiding_an_ancestor_of_the_focus_widget_moves_the_focus() {
    let (_root, a, b, c, d, _log) = focus_tree();
    set_widget_visible(&c, false);
    assert_eq!(d.focus_manager().focused_widget_id(), Some(b.borrow().id()));
    assert!(!a.borrow().has_focus());
}

#[test]
fn hiding_the_only_focus_widget_clears_the_focus() {
    let (_root, a, b, _c, d, _log) = focus_tree();
    set_widget_visible(&b, false);
    assert_eq!(
        d.focus_manager().focused_widget_id(),
        Some(a.borrow().id()),
        "an unrelated hide keeps it"
    );
    set_widget_visible(&a, false);
    assert_eq!(d.focus_manager().focused_widget_id(), None);
    assert!(!a.borrow().has_focus());
}

/// A window holding a stack (page 0: `a0`, `a1`; page 1: `x` with no focus, then `b1`) and,
/// after it, a widget `o` outside the stack. `focus` gets the focus.
fn stack_window(
    focus: &str,
) -> (
    WidgetRef,
    WidgetRef,
    Vec<WidgetRef>,
    EventTreeDispatcher,
    Log,
) {
    let log = Log::default();
    let root = probe("root", FocusPolicy::NoFocus, &log);
    let s = stack();
    let page0 = probe("p0", FocusPolicy::NoFocus, &log);
    let page1 = probe("p1", FocusPolicy::NoFocus, &log);
    let ws: Vec<_> = [
        ("a0", FocusPolicy::StrongFocus),
        ("a1", FocusPolicy::StrongFocus),
        ("x", FocusPolicy::NoFocus),
        ("b1", FocusPolicy::StrongFocus),
        ("o", FocusPolicy::StrongFocus),
    ]
    .into_iter()
    .map(|(n, p)| probe(n, p, &log))
    .collect();
    page0.borrow_mut().add_child(ws[0].clone());
    page0.borrow_mut().add_child(ws[1].clone());
    page1.borrow_mut().add_child(ws[2].clone());
    page1.borrow_mut().add_child(ws[3].clone());
    add(&s, &page0);
    add(&s, &page1);
    root.borrow_mut().add_child(s.clone());
    root.borrow_mut().add_child(ws[4].clone());
    adopt_tree(&root);
    let mut d = EventTreeDispatcher::new();
    let target = ws
        .iter()
        .find(|w| probe_name(w) == focus)
        .unwrap()
        .borrow()
        .id();
    assert!(d
        .focus_manager_mut()
        .set_focus(&root, Some(target), FocusReason::Other));
    log.borrow_mut().clear();
    let mut all = vec![page0, page1];
    all.extend(ws);
    (root, s, all, d, log)
}

fn probe_name(w: &WidgetRef) -> &'static str {
    w.borrow().as_any().downcast_ref::<Probe>().unwrap().name
}

fn focused(d: &EventTreeDispatcher, ws: &[WidgetRef]) -> Option<&'static str> {
    let id = d.focus_manager().focused_widget_id()?;
    ws.iter().find(|w| w.borrow().id() == id).map(probe_name)
}

fn focus_log(log: &Log) -> Vec<String> {
    log.borrow()
        .iter()
        .filter(|e| e.contains(":In:") || e.contains(":Out:"))
        .cloned()
        .collect()
}

/// `QStackedLayout::setCurrentIndex` (qstackedlayout.cpp:291-336): `prev->clearFocus()` does not
/// touch a focus widget below the page, so hiding the page runs `hide_helper`'s
/// `focusNextChild()` first (qwidget.cpp:8226-8238), which lands outside the stack here; then the
/// focus moves to the new page's first Tab child.
#[test]
fn a_page_switch_moves_focus_below_the_old_page_out_then_into_the_new_page() {
    let (_root, s, ws, d, log) = stack_window("a0");
    StackedWidget::set_current_index(&s, 1);
    assert_eq!(focused(&d, &ws), Some("b1"));
    assert_eq!(
        focus_log(&log),
        ["a0:Out:Tab", "o:In:Tab", "o:Out:Other", "b1:In:Other"]
    );
    assert!(!ws[6].borrow().has_focus(), "o does not keep the focus");
}

/// With nothing outside the stack to take the handover, the focus is cleared and then set into
/// the new page.
#[test]
fn a_page_switch_with_no_widget_outside_the_stack_still_focuses_the_new_page() {
    let (_root, s, ws, d, log) = stack_window("a0");
    set_widget_enabled(&ws[6], false);
    log.borrow_mut().clear();
    StackedWidget::set_current_index(&s, 1);
    assert_eq!(focused(&d, &ws), Some("b1"));
}

/// The page itself holding the focus: `isAncestorOf` counts the widget itself
/// (qwidget.cpp `QWidget::isAncestorOf`: `child == this` returns true), so `clearFocus()` runs and
/// the focus then moves into the new page.
#[test]
fn focus_on_the_page_itself_moves_into_the_new_page() {
    let (root, s, ws, mut d, log) = stack_window("a0");
    let page0 = ws[0].borrow().id();
    assert!(d
        .focus_manager_mut()
        .set_focus(&root, Some(page0), FocusReason::Other));
    log.borrow_mut().clear();
    StackedWidget::set_current_index(&s, 1);
    assert_eq!(focused(&d, &ws), Some("b1"));
}

/// Switching back gives the focus to the page's own focus widget (`next->focusWidget()`).
#[test]
fn switching_back_restores_the_pages_focus_widget() {
    let (_root, s, ws, d, _log) = stack_window("a1");
    StackedWidget::set_current_index(&s, 1);
    StackedWidget::set_current_index(&s, 0);
    assert_eq!(focused(&d, &ws), Some("a1"));
}

/// A page with no Tab child gets the focus itself (third best, qstackedlayout.cpp:331-332).
#[test]
fn a_page_without_a_tab_child_gets_the_focus_itself() {
    let (_root, s, ws, d, _log) = stack_window("a0");
    set_widget_enabled(&ws[5], false);
    StackedWidget::set_current_index(&s, 1);
    assert_eq!(focused(&d, &ws), Some("p1"));
}

/// Focus outside the stack stays where it is.
#[test]
fn a_page_switch_leaves_focus_outside_the_stack() {
    let (_root, s, ws, d, log) = stack_window("o");
    StackedWidget::set_current_index(&s, 1);
    assert_eq!(focused(&d, &ws), Some("o"));
    assert!(focus_log(&log).is_empty());
}

/// `setCurrentIndex(currentIndex())` returns at once (qstackedlayout.cpp:295-297): an explicitly
/// hidden current page stays hidden and no signal is emitted.
#[test]
fn setting_the_current_index_again_is_a_no_op() {
    let log = Log::default();
    let mut layout = StackedLayout::new();
    let page = probe("p", FocusPolicy::NoFocus, &log);
    layout.add_widget(page.clone());
    set_widget_visible(&page, false);
    layout.set_current_index(0);
    assert!(!page.borrow().is_visible());

    let s = stack();
    add(&s, &page);
    set_widget_visible(&page, false);
    let fired = Arc::new(Mutex::new(Vec::<i64>::new()));
    let f = fired.clone();
    s.borrow()
        .as_any()
        .downcast_ref::<StackedWidget>()
        .unwrap()
        .current_changed
        .connect(move |i| f.lock().unwrap().push(*i as i64));
    StackedWidget::set_current_index(&s, 0);
    assert!(!page.borrow().is_visible());
    assert!(fired.lock().unwrap().is_empty());
}

/// `QStackedLayout::takeAt` (qstackedlayout.cpp:240-262): removing the current page shows the
/// neighbour and announces it before the removed page is hidden; removing the last page
/// announces -1.
#[test]
fn removing_pages_announces_the_new_current_then_hides() {
    let log = Log::default();
    let s = stack();
    let (a, b) = (
        probe("a", FocusPolicy::NoFocus, &log),
        probe("b", FocusPolicy::NoFocus, &log),
    );
    add(&s, &a);
    add(&s, &b);
    let fired = Arc::new(Mutex::new(Vec::<i64>::new()));
    let f = fired.clone();
    s.borrow()
        .as_any()
        .downcast_ref::<StackedWidget>()
        .unwrap()
        .current_changed
        .connect(move |i| f.lock().unwrap().push(*i as i64));
    // Each page's handler logs the signals emitted before it ran.
    for w in [&a, &b] {
        let (l, f) = (log.clone(), fired.clone());
        set_hook(w, move || {
            l.borrow_mut().push(format!("seen:{:?}", f.lock().unwrap()))
        });
    }
    log.borrow_mut().clear();
    StackedWidget::remove_widget(&s, 0);
    assert_eq!(*log.borrow(), ["b:Show", "seen:[]", "a:Hide", "seen:[0]"]);
    log.borrow_mut().clear();
    StackedWidget::remove_widget(&s, 0);
    assert_eq!(*log.borrow(), ["b:Hide", "seen:[0, -1]"]);
}

/// `hide_helper` stops at `isWindow()` (qwidget.cpp:8184): hiding the window keeps its focus
/// widget.
#[test]
fn hiding_a_window_keeps_its_focus_widget() {
    let (root, a, _b, _c, d, _log) = focus_tree();
    set_widget_visible(&root, false);
    assert_eq!(d.focus_manager().focused_widget_id(), Some(a.borrow().id()));
}

/// Disabling a container from a child's own handler: the child is borrowed, so it is disabled
/// once its handler returns.
#[test]
fn disabling_a_container_from_a_childs_handler_still_disables_the_child() {
    let log = Log::default();
    let form = probe("form", FocusPolicy::NoFocus, &log);
    let child = probe("child", FocusPolicy::StrongFocus, &log);
    form.borrow_mut().add_child(child.clone());
    adopt_tree(&form);
    {
        let g = child.borrow_mut();
        set_widget_enabled(&form, false);
        drop(g);
    }
    WidgetCommandQueue::flush();
    assert!(!form.borrow().is_enabled());
    assert!(
        !child.borrow().is_enabled(),
        "the borrowed child is disabled too"
    );
}

/// Enabling again before the retry runs leaves the child enabled.
#[test]
fn a_deferred_disable_is_dropped_when_the_parent_is_enabled_again() {
    let log = Log::default();
    let form = probe("form", FocusPolicy::NoFocus, &log);
    let child = probe("child", FocusPolicy::StrongFocus, &log);
    form.borrow_mut().add_child(child.clone());
    adopt_tree(&form);
    {
        let g = child.borrow_mut();
        set_widget_enabled(&form, false);
        set_widget_enabled(&form, true);
        drop(g);
    }
    WidgetCommandQueue::flush();
    assert!(child.borrow().is_enabled());
}

thread_local! {
    static MENU: RefCell<Option<WidgetRef>> = const { RefCell::new(None) };
    static SHARED: RefCell<Option<MenuRef>> = const { RefCell::new(None) };
}

/// `aboutToShow` / `aboutToHide` run with no borrow of the menu held, so a slot can fill the menu
/// (the Qt idiom) when the menu is shown with `set_widget_visible`.
#[test]
fn about_to_show_and_hide_slots_can_borrow_the_menu_through_set_widget_visible() {
    let menu: WidgetRef = Rc::new(RefCell::new(Box::new(Menu::new("m"))));
    MENU.with(|m| *m.borrow_mut() = Some(menu.clone()));
    let hidden = Arc::new(Mutex::new(0));
    {
        let g = menu.borrow();
        let m = g.as_any().downcast_ref::<Menu>().unwrap();
        m.about_to_show.connect(|_| {
            MENU.with(|m| {
                let w = m.borrow();
                let mut g = w.as_ref().unwrap().borrow_mut();
                g.as_any_mut()
                    .downcast_mut::<Menu>()
                    .unwrap()
                    .add_action(Action::new_ref("filled"));
            });
        });
        let h = Arc::clone(&hidden);
        m.about_to_hide.connect(move |_| {
            MENU.with(|m| {
                let _ = m.borrow().as_ref().unwrap().borrow_mut();
            });
            *h.lock().unwrap() += 1;
        });
    }
    set_widget_visible(&menu, true);
    assert!(menu.borrow().is_visible());
    set_widget_visible(&menu, false);
    assert_eq!(*hidden.lock().unwrap(), 1, "emitted once");
    let g = menu.borrow();
    assert_eq!(
        g.as_any().downcast_ref::<Menu>().unwrap().actions().len(),
        1
    );
    MENU.with(|m| *m.borrow_mut() = None);
}

/// `popup_at` and `exec_popup` emit `aboutToShow` with the menu free; the slot fills it
/// and the popup then sizes itself with the new action.
#[test]
fn popup_emits_about_to_show_with_the_menu_free() {
    let menu = Menu::new_ref("m");
    SHARED.with(|s| *s.borrow_mut() = Some(menu.clone()));
    menu.borrow().about_to_show.connect(|_| {
        SHARED.with(|s| {
            s.borrow()
                .as_ref()
                .unwrap()
                .borrow_mut()
                .add_action(Action::new_ref("filled"));
        });
    });
    Menu::popup_at(&menu, Point::new(5, 5), None);
    assert!(menu.borrow().is_visible());
    assert_eq!(menu.borrow().actions().len(), 1);
    Menu::hide_menu(&menu);
    assert!(!menu.borrow().is_visible());
    // Windows' `exec_popup` runs a native modal loop.
    #[cfg(not(windows))]
    {
        assert!(Menu::exec_popup(&menu, Point::new(5, 5)).is_none());
        assert_eq!(menu.borrow().actions().len(), 2);
    }
    SHARED.with(|s| *s.borrow_mut() = None);
}

fn click_outside() -> Event {
    Event::new_spontaneous(EventKind::MouseButtonPress {
        x: 900,
        y: 900,
        button: 1,
    })
}

fn menu_with_hide_slot() -> (MenuRef, Arc<Mutex<u32>>) {
    let menu = Menu::new_ref("m");
    menu.borrow_mut().add_new_action("item");
    SHARED.with(|s| *s.borrow_mut() = Some(menu.clone()));
    let hidden = Arc::new(Mutex::new(0));
    let h = Arc::clone(&hidden);
    menu.borrow().about_to_hide.connect(move |_| {
        // Borrowing the menu mutably panics when the emitter still holds it.
        SHARED.with(|s| drop(s.borrow().as_ref().unwrap().borrow_mut()));
        *h.lock().unwrap() += 1;
    });
    Menu::popup(&menu, Point::new(5, 5));
    (menu, hidden)
}

/// A click outside dismisses the menu through `Menu::dispatch`, which emits `about_to_hide` with
/// the menu not borrowed.
#[test]
fn a_click_outside_emits_about_to_hide_with_the_menu_free() {
    let (menu, hidden) = menu_with_hide_slot();
    Menu::dispatch(&menu, &mut click_outside());
    assert!(!menu.borrow().is_visible());
    assert_eq!(*hidden.lock().unwrap(), 1);
    SHARED.with(|s| *s.borrow_mut() = None);
}

/// The same click delivered by the event dispatcher, which holds the menu borrowed while
/// `event` runs: the signal is emitted at the next flush, once the borrow is gone.
#[test]
fn a_click_outside_through_event_emits_about_to_hide_after_the_borrow() {
    let (menu, hidden) = menu_with_hide_slot();
    menu.borrow_mut().event(&mut click_outside());
    assert!(!menu.borrow().is_visible());
    WidgetCommandQueue::flush();
    assert_eq!(*hidden.lock().unwrap(), 1);
    SHARED.with(|s| *s.borrow_mut() = None);
}

thread_local! {
    static CHAIN: RefCell<Vec<MenuRef>> = const { RefCell::new(Vec::new()) };
}

type Signals = Arc<Mutex<Vec<&'static str>>>;

/// Borrows every menu of the chain mutably: a slot doing this panics while an emitter still holds
/// one of them.
fn touch_chain() {
    CHAIN.with(|c| c.borrow().iter().for_each(|m| drop(m.borrow_mut())));
}

fn key_event(key: u32) -> Event {
    Event::new_spontaneous(EventKind::KeyPress {
        key,
        modifiers: 0,
        is_repeat: false,
    })
}

const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;

/// A top menu whose sub-menu holds one item, popped up with the sub-menu open and the item
/// selected. Every slot borrows both menus and logs itself.
fn open_chain() -> (MenuRef, MenuRef, Signals) {
    WidgetCommandQueue::clear();
    let top = Menu::new_ref("top");
    let sub = top.borrow_mut().add_menu("sub");
    let item = sub.borrow_mut().add_new_action("item");
    CHAIN.with(|c| *c.borrow_mut() = vec![top.clone(), sub.clone()]);
    let log: Signals = Arc::new(Mutex::new(Vec::new()));
    let entry = |name: &'static str| {
        let log = Arc::clone(&log);
        move || {
            touch_chain();
            log.lock().unwrap().push(name);
        }
    };
    let f = entry("sub hide");
    sub.borrow().about_to_hide.connect(move |_| f());
    let f = entry("top hide");
    top.borrow().about_to_hide.connect(move |_| f());
    let f = entry("item triggered");
    item.borrow().triggered.connect(move |_| f());
    let f = entry("sub triggered");
    sub.borrow().triggered.connect(move |_| f());
    let f = entry("top triggered");
    top.borrow().triggered.connect(move |_| f());
    Menu::popup(&top, Point::new(0, 0));
    Menu::dispatch(&top, &mut key_event(0x28)); // Down selects the sub-menu item
    Menu::dispatch(&top, &mut key_event(0x27)); // Right opens it and selects the item
    assert!(sub.borrow().is_visible());
    (top, sub, log)
}

const ACTIVATION: [&str; 5] = [
    "sub hide",
    "top hide",
    "item triggered",
    "sub triggered",
    "top triggered",
];

/// Choosing an item of a sub-menu hides the chain deepest first and then triggers the action
/// (`QMenuPrivate::activateAction`: `hideUpToMenuBar`, then `activate`), all with no menu borrowed.
#[test]
fn activating_a_submenu_item_hides_the_chain_then_triggers_with_the_menus_free() {
    let (top, sub, log) = open_chain();
    Menu::dispatch(&top, &mut key_event(VK_RETURN));
    assert!(!top.borrow().is_visible() && !sub.borrow().is_visible());
    assert_eq!(*log.lock().unwrap(), ACTIVATION);
    CHAIN.with(|c| c.borrow_mut().clear());
}

/// The same activation delivered by the event dispatcher, which holds the top menu borrowed while
/// `event` runs: the signals follow at the next flush, in the same order.
#[test]
fn activating_through_event_announces_the_chain_at_the_next_flush() {
    let (top, sub, log) = open_chain();
    top.borrow_mut().event(&mut key_event(VK_RETURN));
    assert!(!top.borrow().is_visible() && !sub.borrow().is_visible());
    assert!(
        log.lock().unwrap().is_empty(),
        "nothing is emitted under the borrow"
    );
    WidgetCommandQueue::flush();
    assert_eq!(*log.lock().unwrap(), ACTIVATION);
    CHAIN.with(|c| c.borrow_mut().clear());
}

/// Escape closes only the sub-menu, whose `about_to_hide` is emitted at once; a second Escape
/// closes the top menu.
#[test]
fn escape_in_a_submenu_announces_its_hide() {
    let (top, sub, log) = open_chain();
    Menu::dispatch(&top, &mut key_event(VK_ESCAPE));
    assert!(top.borrow().is_visible() && !sub.borrow().is_visible());
    assert_eq!(*log.lock().unwrap(), ["sub hide"]);
    Menu::dispatch(&top, &mut key_event(VK_ESCAPE));
    assert!(!top.borrow().is_visible());
    assert_eq!(*log.lock().unwrap(), ["sub hide", "top hide"]);
    CHAIN.with(|c| c.borrow_mut().clear());
}

/// `hide_helper`'s loop stops at `isWindow()`, not at a widget whose parent link is missing: a
/// container hidden before `adopt_tree` ran still hands its focus widget on.
#[test]
fn hiding_an_unadopted_container_moves_the_focus() {
    let log = Log::default();
    let root = probe("root", FocusPolicy::NoFocus, &log);
    let a = probe("a", FocusPolicy::StrongFocus, &log);
    let b = probe("b", FocusPolicy::StrongFocus, &log);
    let c = probe("c", FocusPolicy::NoFocus, &log);
    root.borrow_mut().add_child(b.clone());
    adopt_tree(&root);
    // `c` joins after the adoption, so it and `a` have no parent link.
    c.borrow_mut().add_child(a.clone());
    root.borrow_mut().add_child(c.clone());
    let mut d = EventTreeDispatcher::new();
    let id = a.borrow().id();
    assert!(d
        .focus_manager_mut()
        .set_focus(&root, Some(id), FocusReason::Other));
    assert!(
        c.borrow().parent_widget().is_none(),
        "precondition: c is not adopted"
    );

    set_widget_visible(&c, false);

    assert_eq!(d.focus_manager().focused_widget_id(), Some(b.borrow().id()));
    assert!(!a.borrow().has_focus());
}
