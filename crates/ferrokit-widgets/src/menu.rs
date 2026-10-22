// SPDX-License-Identifier: LGPL-3.0-only
//! Popup menus with keyboard navigation and cascading sub-menus (`QMenu`).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::notify_borrowed;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Point, PointF, Rect, RectF, Size};
use ferrokit_gui::image::{Icon, IconMode, IconState};
use ferrokit_gui::paint::brush::Brush;
use ferrokit_gui::paint::painter::{Painter, Pen, RenderHint};
use ferrokit_gui::text::{Font, FontMetrics};
use ferrokit_gui::tiny_skia::Color;

use crate::action::{keys, Action, ActionRef, ActionWeak};
use crate::focus::FocusPolicy;
use crate::layout::Layout;
use crate::size_policy::{Policy, QSizePolicy};
use crate::style::stylesheet::{ResolvedStyle, WidgetStyleContext};
use crate::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};

/// Shared handle to a [`Menu`]; sub-menus, menu bars and tool bar extensions hold menus this way.
pub type MenuRef = Rc<RefCell<Menu>>;

const FRAME: i32 = 1;
const V_PADDING: i32 = 4;
const SEPARATOR_HEIGHT: i32 = 9;
const CHECK_COLUMN: i32 = 28;
const SHORTCUT_GAP: i32 = 28;
const ARROW_COLUMN: i32 = 20;
const RIGHT_PADDING: i32 = 8;
const MIN_WIDTH: i32 = 120;
const SUBMENU_OVERLAP: i32 = 2;
const ICON_SIZE: i32 = 16;
/// Extra width `QStyleSheetStyle` adds after the check indicator in `CT_MenuItem`.
const CHECK_GAP: i32 = 4;

fn native_style() -> ferrokit_platform::NativeStyle {
    ferrokit_platform::platform().theme().native_style()
}

/// Width and height of the check indicator box: the base style's `PM_IndicatorWidth` /
/// `PM_IndicatorHeight`. A styled menu with checkable items reserves this plus [`CHECK_GAP`]
/// before the text (`QStyleSheetStyle::sizeFromContents`, `CT_MenuItem`), and draws the text this
/// far from the item's padding edge (`drawControl`, `CE_MenuItem`: `textRectOffset`).
///
/// `windows11` 16 (`QWindows11Style::pixelMetric`), `Fusion` 14 (`QFusionStyle::pixelMetric`),
/// `windowsvista` 13 (the theme's check box size at 96 dpi, queried from uxtheme by Qt),
/// `macos` 18 + 1 (`CheckBoxWidth` + 1 in `qmacstyle_mac.mm`, the default control size).
/// All but `macos` were confirmed against PySide6's menu widths.
fn check_indicator() -> i32 {
    use ferrokit_platform::NativeStyle;
    match native_style() {
        NativeStyle::Windows11 => 16,
        NativeStyle::WindowsVista => 13,
        NativeStyle::Fusion => 14,
        NativeStyle::Macintosh => 19,
    }
}

/// Box model and palette of a style-sheet-driven menu (`QMenu`, `QMenu::item`,
/// `QMenu::separator` rules). Without it a menu keeps its built-in look.
///
/// Geometry follows `QStyleSheetStyle`: a row is as tall as the *menu font* (`row_font`,
/// not the item's `font-size`) plus the item padding, a menu is as wide as the widest text
/// in `row_font` plus left/right padding and 20 px for the check column when any item is
/// checkable, and the first row starts below the 1 px frame and the `QMenu` padding.
#[derive(Clone, Debug)]
pub struct MenuStyle {
    /// Font the item text is drawn with (the `QMenu::item` `font-size`).
    pub font: Font,
    /// The menu's own font, which sizes rows and text widths.
    pub row_font: Font,
    pub background: Color,
    pub border: Color,
    pub text: Color,
    pub disabled_text: Color,
    pub hover_background: Color,
    pub hover_text: Color,
    pub separator: Color,
    pub radius: f32,
    /// `QMenu { padding }` above the first and below the last row.
    pub padding_v: i32,
    /// `QMenu::item { padding }` as top, right, bottom, left.
    pub item_padding: [i32; 4],
    /// `QMenu::separator { margin }` as vertical, horizontal; the line itself is 1 px.
    pub separator_margin: [i32; 2],
}

/// Result of routing one input event through a chain of open menus.
pub(crate) enum MenuOutcome {
    /// The event was not used by the menu chain.
    Ignored,
    /// The event was consumed.
    Handled,
    /// The receiving menu closed itself (Escape or click outside).
    Closed,
    /// Left was pressed in the deepest menu with no sub-menu to close.
    NavigateLeft,
    /// Right was pressed on an item without a sub-menu.
    NavigateRight,
    /// An action was chosen. All menus up to the receiver are already hidden; the
    /// signals are the `triggered` signals of those menus, deepest first.
    Activate(ActionRef, Vec<Signal<ActionRef>>),
}

impl MenuOutcome {
    /// Triggers `action` and then emits every collected menu `triggered` signal.
    pub(crate) fn fire(action: &ActionRef, signals: &[Signal<ActionRef>]) {
        Action::trigger(action);
        for signal in signals {
            signal.emit(action);
        }
    }
}

/// Hides a sub-menu its parent holds. `aboutToHide` is emitted before the sub-menu is borrowed
/// mutably, so a slot can reach the sub-menu through its own handle.
fn hide_shared(submenu: &MenuRef) {
    let signal = match submenu.try_borrow() {
        Ok(sub) if sub.base.is_visible() => Some(sub.about_to_hide.clone()),
        _ => None,
    };
    if let Some(signal) = signal {
        signal.emit(&());
    }
    hide_quietly(submenu);
}

/// Hides `submenu` without emitting `aboutToHide`; while it is borrowed (e.g. a slot is running
/// on it) the hide is retried at the next flush.
fn hide_quietly(submenu: &MenuRef) {
    match submenu.try_borrow_mut() {
        Ok(mut sub) => sub.hide_menu_emitting(false),
        Err(_) => {
            let weak = std::rc::Rc::downgrade(submenu);
            crate::command::defer_retry(move || {
                if let Some(submenu) = weak.upgrade() {
                    hide_quietly(&submenu);
                }
            });
        }
    }
}

/// Releases the mouse capture when a popup loop ends, also when it unwinds.
#[cfg(windows)]
struct CaptureGuard;

#[cfg(windows)]
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture() };
    }
}

/// Popup menu (`QMenu`).
///
/// A menu paints and routes events for its open sub-menu itself, so a whole cascade
/// behaves as one widget. Sub-menu geometry is relative to the parent menu's origin.
pub struct Menu {
    base: WidgetBase,
    title: String,
    icon: Icon,
    actions: Vec<ActionRef>,
    active: Option<usize>,
    open_submenu: Option<(usize, MenuRef)>,
    menu_action: ActionWeak,
    popup_bounds: Option<Rect>,
    last_triggered: Option<ActionRef>,
    /// Previous pointer position over this menu, used to tell whether the pointer is heading
    /// for the open submenu. Cleared when the pointer leaves or enters a submenu.
    last_mouse_pos: Option<Point>,
    last_covered: std::cell::Cell<Rect>,
    font: Font,
    style: Option<MenuStyle>,

    background_color: Color,
    border_color: Color,
    text_color: Color,
    disabled_text_color: Color,
    highlight_color: Color,
    highlight_text_color: Color,
    separator_color: Color,

    /// Emitted with the action chosen in this menu or one of its sub-menus.
    pub triggered: Signal<ActionRef>,
    /// Emitted with the action highlighted by mouse or keyboard.
    pub hovered: Signal<ActionRef>,
    /// Emitted just before the menu is shown.
    pub about_to_show: Signal<()>,
    /// Emitted just before the menu is hidden.
    pub about_to_hide: Signal<()>,
    /// `set_widget_visible` already emitted the signal of the pending `set_visible`.
    announced: std::cell::Cell<bool>,
}

pub type QMenu = Menu;

impl Menu {
    /// Creates a hidden menu with the given title (used by menu bars and parent menus).
    pub fn new(title: impl Into<String>) -> Self {
        let base = WidgetBase::new();
        base.set_visible(false);
        base.clear_dirty();
        base.set_focus_policy(FocusPolicy::StrongFocus);
        base.set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Fixed));
        // QMenuPrivate::init: setMouseTracking(SH_Menu_MouseTracking), true in the common and
        // Windows styles (qmenu.cpp:172, qcommonstyle.cpp:5411, qwindowsstyle.cpp:463).
        base.set_mouse_tracking(true);
        base.set_geometry(Rect::new(0, 0, MIN_WIDTH, 2 * V_PADDING));
        let triggered = Signal::with_owner(&base.object_data);
        let hovered = Signal::with_owner(&base.object_data);
        let about_to_show = Signal::with_owner(&base.object_data);
        let about_to_hide = Signal::with_owner(&base.object_data);
        Self {
            base,
            title: title.into(),
            icon: Icon::default(),
            actions: Vec::new(),
            active: None,
            open_submenu: None,
            menu_action: Weak::new(),
            popup_bounds: None,
            last_triggered: None,
            last_mouse_pos: None,
            last_covered: std::cell::Cell::new(Rect::default()),
            font: Font::new("Segoe UI, Microsoft JhengHei, Segoe UI Emoji", 12.0),
            style: None,

            background_color: Color::from_rgba8(22, 25, 32, 250),
            border_color: Color::from_rgba8(255, 255, 255, 46),
            text_color: Color::from_rgba8(226, 232, 240, 255),
            disabled_text_color: Color::from_rgba8(100, 116, 139, 255),
            highlight_color: Color::from_rgba8(39, 47, 61, 255),
            highlight_text_color: Color::from_rgba8(56, 189, 248, 255),
            separator_color: Color::from_rgba8(255, 255, 255, 31),

            triggered,
            hovered,
            about_to_show,
            about_to_hide,
            announced: std::cell::Cell::new(false),
        }
    }

    /// Creates a new shared menu.
    pub fn new_ref(title: impl Into<String>) -> MenuRef {
        Rc::new(RefCell::new(Self::new(title)))
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Sets the title and keeps the menu action text in sync.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
        if let Some(action) = self.menu_action.upgrade() {
            if let Ok(mut a) = action.try_borrow_mut() {
                a.set_text(self.title.clone());
            }
        }
        self.update();
    }
    /// Sets dark or light mode palette matching Qt / Python style.
    pub fn set_dark_mode(&mut self, dark: bool) {
        if dark {
            self.background_color = Color::from_rgba8(22, 25, 32, 250);
            self.border_color = Color::from_rgba8(255, 255, 255, 46);
            self.text_color = Color::from_rgba8(226, 232, 240, 255);
            self.disabled_text_color = Color::from_rgba8(100, 116, 139, 255);
            self.highlight_color = Color::from_rgba8(39, 47, 61, 255);
            self.highlight_text_color = Color::from_rgba8(56, 189, 248, 255);
            self.separator_color = Color::from_rgba8(255, 255, 255, 31);
        } else {
            self.background_color = Color::from_rgba8(250, 250, 250, 255);
            self.border_color = Color::from_rgba8(160, 160, 160, 255);
            self.text_color = Color::from_rgba8(20, 20, 20, 255);
            self.disabled_text_color = Color::from_rgba8(160, 160, 160, 255);
            self.highlight_color = Color::from_rgba8(0, 120, 215, 255);
            self.highlight_text_color = Color::from_rgba8(255, 255, 255, 255);
            self.separator_color = Color::from_rgba8(210, 210, 210, 255);
        }
        for action in &self.actions {
            if let Some(sub) = action.borrow().menu() {
                if let Ok(mut s) = sub.try_borrow_mut() {
                    s.set_dark_mode(dark);
                }
            }
        }
        self.update();
    }

    pub fn is_dark_mode(&self) -> bool {
        self.background_color.red() < 0.5
    }
    pub fn icon(&self) -> &Icon {
        &self.icon
    }

    pub fn set_icon(&mut self, icon: Icon) {
        self.icon = icon.clone();
        if let Some(action) = self.menu_action.upgrade() {
            if let Ok(mut a) = action.try_borrow_mut() {
                a.set_icon(icon);
            }
        }
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    pub fn set_font(&mut self, font: Font) {
        self.font = font;
        // FontChange (qwidget.cpp:4894-4895) precedes the update work (qwidget.cpp:9502).
        notify_borrowed(self, &mut Event::new(EventKind::FontChange));
        self.update();
    }

    /// Applies a style-sheet-like look (colours, fonts and box model) to this menu and, when
    /// they open, its sub-menus.
    pub fn set_style(&mut self, style: MenuStyle) {
        self.font = style.font.clone();
        self.background_color = style.background;
        self.border_color = style.border;
        self.text_color = style.text;
        self.disabled_text_color = style.disabled_text;
        self.highlight_color = style.hover_background;
        self.highlight_text_color = style.hover_text;
        self.separator_color = style.separator;
        self.style = Some(style);
        self.update();
    }

    fn top_inset(&self) -> i32 {
        self.effective_style()
            .map_or(V_PADDING, |s| FRAME + s.padding_v)
    }

    /// `QMenu` rules for one sub-control and pseudo-state list, resolved like Qt's
    /// `QStyleSheetStyle::renderRule`: every matching rule cascades and the more specific one
    /// wins, so `QMenu::item:selected` without a `color` keeps the `QMenu::item` colour.
    fn qss(&self, sub_control: Option<&'static str>, states: &[&'static str]) -> ResolvedStyle {
        let props = self.base.properties.borrow();
        self.base.resolve_style(&WidgetStyleContext {
            type_name: "QMenu",
            object_name: "",
            pseudo_states: states,
            sub_control,
            attributes: props.as_slice(),
        })
    }

    /// The base for a menu with no explicit `MenuStyle`: the `set_dark_mode` colours with the
    /// unstyled metrics, so a `QMenu` rule alone still yields a complete styled menu.
    fn hard_style(&self) -> MenuStyle {
        MenuStyle {
            font: self.font.clone(),
            row_font: self.font.clone(),
            background: self.background_color,
            border: self.border_color,
            text: self.text_color,
            disabled_text: self.disabled_text_color,
            hover_background: self.highlight_color,
            hover_text: self.highlight_text_color,
            separator: self.separator_color,
            radius: 6.0,
            padding_v: V_PADDING,
            item_padding: [V_PADDING, RIGHT_PADDING, V_PADDING, CHECK_COLUMN - FRAME],
            separator_margin: [4, 8],
        }
    }

    /// The style the menu is laid out with: the explicit `MenuStyle` (or `hard_style`) with every
    /// `QMenu` rule from the sheets above it applied on top. `None` when there is neither, which
    /// keeps the unstyled path.
    fn effective_style(&self) -> Option<MenuStyle> {
        let menu = self.qss(None, &[]);
        let item = self.qss(Some("item"), &[]);
        let selected = self.qss(Some("item"), &["selected"]);
        let disabled = self.qss(Some("item"), &["disabled"]);
        let separator = self.qss(Some("separator"), &[]);
        let declared = [&menu, &item, &selected, &disabled, &separator]
            .iter()
            .any(|rule| **rule != ResolvedStyle::default());
        if self.style.is_none() && !declared {
            return None;
        }
        let mut s = self.style.clone().unwrap_or_else(|| self.hard_style());
        if let Some(c) = menu.background_color {
            s.background = c;
        }
        if let Some(c) = menu.border_color {
            s.border = c;
        }
        if let Some(r) = menu.border_radius {
            s.radius = r;
        }
        if let Some(p) = menu.padding {
            s.padding_v = p[0].round() as i32;
        }
        if let Some(p) = item.padding {
            s.item_padding = p.map(|v| v.round() as i32);
        }
        if let Some(sz) = item.font_size {
            s.font.size = sz.round();
        }
        if let Some(c) = item.color {
            s.text = c;
        }
        if let Some(c) = disabled.color {
            s.disabled_text = c;
        }
        if let Some(c) = selected.background_color {
            s.hover_background = c;
        }
        if let Some(c) = selected.color {
            s.hover_text = c;
        }
        if let Some(c) = separator.background_color {
            s.separator = c;
        }
        if let Some(m) = separator.margin {
            s.separator_margin = [m[0].round() as i32, m[1].round() as i32];
        }
        Some(s)
    }

    fn separator_height(&self) -> i32 {
        self.effective_style()
            .map_or(SEPARATOR_HEIGHT, |s| 1 + 2 * s.separator_margin[0])
    }

    fn has_checkable(&self) -> bool {
        self.actions.iter().any(|a| {
            let a = a.borrow();
            a.is_visible() && !a.is_separator() && a.is_checkable()
        })
    }

    /// X of the item text in menu coordinates.
    fn text_x(&self) -> f32 {
        match self.effective_style() {
            Some(s) => {
                (FRAME
                    + s.item_padding[3]
                    + if self.has_checkable() {
                        check_indicator()
                    } else {
                        0
                    }) as f32
            }
            None => CHECK_COLUMN as f32,
        }
    }

    fn measure_font(&self) -> Font {
        self.effective_style()
            .map_or_else(|| self.font.clone(), |s| s.row_font)
    }

    fn width_for(&self, max_text: i32, shortcut_w: i32) -> i32 {
        match self.effective_style() {
            Some(s) => {
                2 * FRAME
                    + s.item_padding[1]
                    + s.item_padding[3]
                    + if self.has_checkable() {
                        check_indicator() + CHECK_GAP
                    } else {
                        0
                    }
                    + max_text
                    + shortcut_w
            }
            None => {
                (CHECK_COLUMN + max_text + shortcut_w + ARROW_COLUMN + RIGHT_PADDING).max(MIN_WIDTH)
            }
        }
    }

    /// Returns the action that represents `menu` inside a menu bar or parent menu
    /// (`QMenu::menuAction`), creating it on first use.
    ///
    /// The owner of the returned action keeps it alive; the menu holds only a weak link.
    pub fn menu_action(menu: &MenuRef) -> ActionRef {
        if let Some(action) = menu.borrow().menu_action.upgrade() {
            return action;
        }
        let (title, icon) = {
            let m = menu.borrow();
            (m.title.clone(), m.icon.clone())
        };
        let action = Action::new_ref(title);
        {
            let mut a = action.borrow_mut();
            a.set_icon(icon);
            a.set_menu(Some(menu.clone()));
        }
        menu.borrow_mut().menu_action = Rc::downgrade(&action);
        action
    }

    /// Appends an action; an action already in the menu is moved to the end.
    pub fn add_action(&mut self, action: ActionRef) -> ActionRef {
        self.remove_action(&action);
        self.actions.push(action.clone());
        self.update();
        action
    }

    /// Creates an action with `text` and appends it (`QMenu::addAction(text)`).
    pub fn add_new_action(&mut self, text: impl Into<String>) -> ActionRef {
        self.add_action(Action::new_ref(text))
    }

    /// Appends a separator action.
    pub fn add_separator(&mut self) -> ActionRef {
        self.add_action(Action::separator_ref())
    }

    /// Creates a sub-menu with `title`, appends its menu action and returns it.
    pub fn add_menu(&mut self, title: impl Into<String>) -> MenuRef {
        let submenu = Menu::new_ref(title);
        self.add_menu_ref(&submenu);
        submenu
    }

    /// Appends the menu action of an existing menu.
    pub fn add_menu_ref(&mut self, menu: &MenuRef) -> ActionRef {
        let action = Menu::menu_action(menu);
        self.add_action(action)
    }

    /// Inserts an action before `index` (clamped to the end).
    pub fn insert_action(&mut self, index: usize, action: ActionRef) -> ActionRef {
        self.remove_action(&action);
        let index = index.min(self.actions.len());
        self.actions.insert(index, action.clone());
        self.close_submenu();
        self.active = None;
        self.update();
        action
    }

    /// Removes an action from the menu.
    pub fn remove_action(&mut self, action: &ActionRef) {
        if let Some(pos) = self.actions.iter().position(|a| Rc::ptr_eq(a, action)) {
            if self.open_submenu.as_ref().is_some_and(|(i, _)| *i == pos) {
                self.close_submenu();
            }
            self.actions.remove(pos);
            if let Some((i, _)) = self.open_submenu.as_mut() {
                if *i > pos {
                    *i -= 1;
                }
            }
            self.active = match self.active {
                Some(a) if a == pos => None,
                Some(a) if a > pos => Some(a - 1),
                other => other,
            };
            self.update();
        }
    }

    /// Removes all actions.
    pub fn clear(&mut self) {
        self.close_submenu();
        self.actions.clear();
        self.active = None;
        self.update();
    }

    pub fn actions(&self) -> Vec<ActionRef> {
        self.actions.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Currently highlighted action.
    pub fn active_action(&self) -> Option<ActionRef> {
        self.active.and_then(|i| self.actions.get(i).cloned())
    }

    /// Highlights an action (or clears the highlight), closing an unrelated open sub-menu.
    pub fn set_active_action(&mut self, action: Option<&ActionRef>) {
        let idx = action.and_then(|a| self.actions.iter().position(|x| Rc::ptr_eq(x, a)));
        if self
            .open_submenu
            .as_ref()
            .is_some_and(|(i, _)| Some(*i) != idx)
        {
            self.close_submenu();
        }
        self.set_active_index(idx);
    }

    /// Currently open sub-menu, if any.
    pub fn open_submenu(&self) -> Option<MenuRef> {
        self.open_submenu.as_ref().map(|(_, m)| m.clone())
    }

    /// Action chosen the last time this menu (or a sub-menu) was activated.
    pub fn last_triggered_action(&self) -> Option<ActionRef> {
        self.last_triggered.clone()
    }

    /// Area, in the coordinate space of this menu's geometry, the popup must stay inside.
    pub fn popup_bounds(&self) -> Option<Rect> {
        self.popup_bounds
    }

    pub fn set_popup_bounds(&mut self, bounds: Option<Rect>) {
        self.popup_bounds = bounds;
    }

    /// Computes the geometry `popup(pos)` would use, keeping the menu inside the popup bounds.
    ///
    /// Like `QMenu::popup`, a menu overflowing the bottom edge opens upwards from `pos`
    /// when there is room, and a menu overflowing the right edge is shifted left.
    pub fn popup_geometry(&self, pos: Point) -> Rect {
        let size = self.size_hint();
        let mut x = pos.x;
        let mut y = pos.y;
        if let Some(b) = self.popup_bounds {
            if x + size.width > b.right() {
                x = b.right() - size.width;
            }
            if y + size.height > b.bottom() {
                y = if pos.y - size.height >= b.y {
                    pos.y - size.height
                } else {
                    b.bottom() - size.height
                };
            }
            x = x.max(b.x);
            y = y.max(b.y);
        }
        Rect::new(x, y, size.width, size.height)
    }

    /// Shows the menu at `pos` (`QMenu::popup`).
    pub fn popup(&mut self, pos: Point) {
        self.popup_at(pos, None);
    }

    /// Shows the menu at `pos` with `at` highlighted.
    ///
    /// `about_to_show` is emitted while `self` is borrowed by the caller; a slot that needs the
    /// menu (the Qt idiom of filling it from this signal) must use [`Menu::popup_at_shared`].
    pub fn popup_at(&mut self, pos: Point, at: Option<&ActionRef>) {
        self.about_to_show.emit(&());
        self.popup_at_announced(pos, at);
    }

    /// `popup_at` for a shared menu: `about_to_show` is emitted with the menu not borrowed.
    pub fn popup_at_shared(menu: &MenuRef, pos: Point, at: Option<&ActionRef>) {
        Self::emit_shared(menu, |m| m.about_to_show.clone());
        menu.borrow_mut().popup_at_announced(pos, at);
    }

    /// `popup` for a shared menu (see [`Menu::popup_at_shared`]).
    pub fn popup_shared(menu: &MenuRef, pos: Point) {
        Self::popup_at_shared(menu, pos, None);
    }

    /// `exec_popup` for a shared menu: `about_to_show` is emitted with the menu not borrowed.
    pub fn exec_popup_shared(menu: &MenuRef, pos: Point) -> Option<ActionRef> {
        Self::emit_shared(menu, |m| m.about_to_show.clone());
        menu.borrow_mut().exec_popup_announced(pos)
    }

    /// `hide_menu` for a shared menu: `about_to_hide` is emitted with the menu not borrowed.
    pub fn hide_menu_shared(menu: &MenuRef) {
        let visible = menu.borrow().base.is_visible();
        if visible {
            Self::emit_shared(menu, |m| m.about_to_hide.clone());
        }
        menu.borrow_mut().hide_menu_emitting(false);
    }

    fn emit_shared(menu: &MenuRef, signal: impl Fn(&Menu) -> Signal<()>) {
        let signal = signal(&menu.borrow());
        signal.emit(&());
    }

    fn popup_at_announced(&mut self, pos: Point, at: Option<&ActionRef>) {
        let geometry = self.popup_geometry(pos);
        self.show_with_geometry(geometry);
        if let Some(action) = at {
            let idx = self.actions.iter().position(|a| Rc::ptr_eq(a, action));
            self.set_active_index(idx);
        }
    }

    /// Runs the menu modally (`QMenu::exec`): pops it up at `pos`, feeds it `events`
    /// until it closes, and returns the triggered action.
    ///
    /// `events` may be any (possibly blocking) event source, e.g. a platform event pump.
    pub fn exec<I>(&mut self, pos: Point, events: I) -> Option<ActionRef>
    where
        I: IntoIterator<Item = Event>,
    {
        self.last_triggered = None;
        self.popup(pos);
        for mut event in events {
            self.event(&mut event);
            if !self.base.is_visible() {
                break;
            }
        }
        if self.base.is_visible() {
            self.hide_menu();
        }
        self.last_triggered.clone()
    }
    /// Pops up the menu as a top-level transparent frameless layered window and runs
    /// modally until an action is selected or dismissed (mirrors `QMenu::exec`).
    #[cfg(windows)]
    pub fn exec_popup(&mut self, pos: Point) -> Option<ActionRef> {
        self.about_to_show.emit(&());
        self.exec_popup_announced(pos)
    }

    #[cfg(windows)]
    fn exec_popup_announced(&mut self, pos: Point) -> Option<ActionRef> {
        use crate::window::Window;
        use ferrokit_platform::{platform, WindowFlags};
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, LoadCursorW, PeekMessageW, PostQuitMessage,
            PostThreadMessageW, SetCursor, SetForegroundWindow, TranslateMessage, IDC_ARROW, MSG,
            PM_REMOVE, WM_ACTIVATEAPP, WM_CANCELMODE, WM_CAPTURECHANGED, WM_KEYDOWN, WM_KILLFOCUS,
            WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NULL, WM_RBUTTONDOWN, WM_RBUTTONUP,
            WM_SETCURSOR,
        };

        self.last_triggered = None;
        let size = self.size_hint();
        // `popupGeometry(QGuiApplication::screenAt(p))` (qmenu.cpp:2382, 292-308): the work area of
        // the screen whose geometry contains the point; without one, the primary screen's.
        let s_geom = platform()
            .screens()
            .into_iter()
            .find(|s| s.geometry().contains(pos))
            .unwrap_or_else(|| platform().primary_screen())
            .available_geometry();
        const DESKTOP_MARGIN: i32 = 8;

        let mut x = pos.x;
        let mut y = pos.y;
        if x + size.width > s_geom.right() - DESKTOP_MARGIN {
            x = (s_geom.right() - size.width - DESKTOP_MARGIN).max(s_geom.x + DESKTOP_MARGIN);
        }
        if y + size.height > s_geom.bottom() - DESKTOP_MARGIN {
            y = if pos.y - size.height >= s_geom.y + DESKTOP_MARGIN {
                (pos.y - size.height).min(s_geom.bottom() - size.height - DESKTOP_MARGIN)
            } else {
                (s_geom.bottom() - size.height - DESKTOP_MARGIN).max(s_geom.y + DESKTOP_MARGIN)
            };
        }
        x = x.max(s_geom.x + DESKTOP_MARGIN);
        y = y.max(s_geom.y + DESKTOP_MARGIN);

        let root_origin = Point::new(x, y);
        let initial_rect = Rect::new(x, y, size.width, size.height);
        self.popup_bounds = Some(s_geom.translated(-root_origin.x, -root_origin.y));
        self.popup_at_announced(Point::new(0, 0), None);

        let mut window = match Window::new(
            "",
            initial_rect,
            WindowFlags::FRAMELESS | WindowFlags::TOOL | WindowFlags::STAYS_ON_TOP | WindowFlags::LAYERED,
        ) {
            Ok(w) => w,
            Err(_) => {
                self.hide_menu();
                return None;
            }
        };

        let hwnd = window.native_handle() as HWND;
        unsafe {
            SetForegroundWindow(hwnd);
            SetCapture(hwnd);
        }
        // Released when this function ends, however it ends (also before the loop starts).
        let capture = CaptureGuard;

        // Losing the active window (Alt+Tab, another application) dismisses a popup. That
        // arrives as a sent message, which `GetMessageW` handles without returning, so the
        // window's deactivation flags it and wakes the loop.
        let dismissed = Rc::new(std::cell::Cell::new(false));
        let flag = Rc::clone(&dismissed);
        let thread = unsafe { GetCurrentThreadId() };
        window.set_window_event_handler(move |ev: &mut Event| {
            if matches!(ev.kind, EventKind::WindowDeactivate) {
                flag.set(true);
                unsafe { PostThreadMessageW(thread, WM_NULL, 0, 0) };
            }
        });
        window.show();
        self.present_popup(&mut window, root_origin, size);

        let mut chosen = None;
        let mut quit = None;
        // The cursor position is a native desktop position: it maps through the screen it is on.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            // `WM_QUIT` makes `GetMessageW` return 0: remember it and pass it on when the menu is
            // gone, so the loop that was running when the menu opened still quits; -1 is an
            // error, not a message.
            while {
                let got = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if got == 0 {
                    quit = Some(msg.wParam as i32);
                }
                got > 0
            } {
                let mouse_pt = Point::new(msg.pt.x, msg.pt.y);
                let mouse_screen = ferrokit_platform::screen::win32_screen::from_native_global_point(
                    mouse_pt,
                    hwnd as isize,
                );
                let local_pt = Point::new(mouse_screen.x - root_origin.x, mouse_screen.y - root_origin.y);
                match msg.message {
                    WM_SETCURSOR => {
                        SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_ARROW));
                    }
                    WM_RBUTTONUP => {
                        if !self.covers(local_pt) {
                            break;
                        }
                    }
                    WM_MOUSEMOVE => {
                        let old_signature = self.hover_signature();
                        let outcome = self.handle_mouse_move(local_pt);
                        let new_signature = self.hover_signature();

                        if old_signature != new_signature {
                            self.present_popup(&mut window, root_origin, size);
                        }

                        if let MenuOutcome::Activate(act, signals) = outcome {
                            chosen = Some((act, signals));
                            break;
                        }
                    }
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                        if !self.covers(local_pt) {
                            break;
                        }
                        let outcome = self.handle_mouse_press(local_pt, 1);
                        if let MenuOutcome::Closed = outcome {
                            break;
                        }
                        self.present_popup(&mut window, root_origin, size);
                    }
                    WM_LBUTTONUP => {
                        if !self.covers(local_pt) {
                            break;
                        }
                        let outcome = self.handle_mouse_release(local_pt, 1);
                        match outcome {
                            MenuOutcome::Activate(act, signals) => {
                                chosen = Some((act, signals));
                                break;
                            }
                            MenuOutcome::Closed => break,
                            _ => {
                                self.present_popup(&mut window, root_origin, size);
                            }
                        }
                    }
                    WM_KEYDOWN => {
                        match msg.wParam as u16 {
                            VK_ESCAPE => break,
                            VK_UP => {
                                self.handle_key(keys::UP, 0);
                            }
                            VK_DOWN => {
                                self.handle_key(keys::DOWN, 0);
                            }
                            VK_LEFT => {
                                self.handle_key(keys::LEFT, 0);
                            }
                            VK_RIGHT => {
                                self.handle_key(keys::RIGHT, 0);
                            }
                            VK_RETURN => {
                                let outcome = self.handle_key(keys::RETURN, 0);
                                if let MenuOutcome::Activate(act, signals) = outcome {
                                    chosen = Some((act, signals));
                                    break;
                                }
                            }
                            _ => {}
                        }
                        self.present_popup(&mut window, root_origin, size);
                    }
                    WM_KILLFOCUS => {
                        // Window lost keyboard/window focus (Win+L, Alt+Tab, clicked outside)
                        break;
                    }
                    WM_ACTIVATEAPP => {
                        // Entire application deactivated (wParam == 0 / FALSE)
                        if msg.wParam == 0 {
                            break;
                        }
                    }
                    WM_CAPTURECHANGED => {
                        // Mouse capture revoked by OS (UAC secure desktop, external capture steal)
                        if msg.lParam as HWND != hwnd {
                            break;
                        }
                    }
                    WM_CANCELMODE => {
                        // Windows or host requests cancellation of internal modal loops
                        break;
                    }
                    _ => {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                if !self.base.is_visible() || dismissed.get() {
                    break;
                }
            }
            drop(capture);
            // Discard any residual mouse button messages queued during the modal menu
            let mut discard_msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut discard_msg, std::ptr::null_mut(), WM_LBUTTONDOWN, WM_RBUTTONUP, PM_REMOVE) != 0 {}
        }
        self.hide_menu();
        // Destroying the popup window may pump messages; the quit is re-posted after that.
        drop(window);
        if let Some(code) = quit {
            unsafe { PostQuitMessage(code) };
        }
        // `QMenu::exec` triggers the chosen action (`QMenuPrivate::activateAction`) after the
        // menu is hidden, then returns it.
        if let Some((action, signals)) = &chosen {
            MenuOutcome::fire(action, signals);
        }
        chosen.map(|(action, _)| action)
    }

    /// Repaints the popup window and sizes it to the menu plus its open sub-menus.
    ///
    /// The window sits at the whole native pixel its logical position maps to, and the menu is
    /// painted at the whole native pixel its root maps to through the same screen: the
    /// difference is a whole number of device pixels, so the menu's own origin never moves
    /// relative to the pixel grid when a sub-menu opens or closes and widens the window. A
    /// fractional logical shift would round each glyph of every item differently and make the
    /// letter spacing jump.
    #[cfg(windows)]
    fn present_popup(&mut self, window: &mut crate::window::Window, root_origin: Point, min_size: Size) {
        use ferrokit_platform::high_dpi::to_native_global_point;
        let covered = self.covered_rect();
        let win_pos = Point::new(root_origin.x + covered.x, root_origin.y + covered.y);
        let win_rect = Rect::new(
            win_pos.x,
            win_pos.y,
            covered.width.max(min_size.width),
            covered.height.max(min_size.height),
        );
        let (origin, dpr) = window.native_mapping_for(win_rect);
        let native_root = to_native_global_point(root_origin, origin, dpr);
        let native_win = to_native_global_point(win_pos, origin, dpr);
        window.present_custom_at(win_rect, |painter| {
            painter.save();
            painter.translate_device(native_root.x - native_win.x, native_root.y - native_win.y);
            self.paint_event(painter);
            painter.restore();
        });
    }
    #[cfg(not(windows))]
    pub fn exec_popup(&mut self, pos: Point) -> Option<ActionRef> {
        self.about_to_show.emit(&());
        self.exec_popup_announced(pos)
    }

    #[cfg(not(windows))]
    fn exec_popup_announced(&mut self, pos: Point) -> Option<ActionRef> {
        // No modal loop without a native window system here: an earlier result is not this one.
        self.last_triggered = None;
        self.popup_at_announced(pos, None);
        self.last_triggered.clone()
    }

    /// Hides the menu and every open sub-menu.
    pub fn hide_menu(&mut self) {
        self.hide_menu_emitting(true);
    }

    /// `hide_menu`; `emit` is false when the caller already emitted `about_to_hide`.
    fn hide_menu_emitting(&mut self, emit: bool) {
        if let Some((_, submenu)) = self.open_submenu.take() {
            hide_shared(&submenu);
        }
        if !self.base.is_visible() {
            return;
        }
        if emit {
            self.about_to_hide.emit(&());
        }
        self.base.set_visible(false);
        self.active = None;
        self.update();
    }

    /// Geometry of an action's item in menu coordinates.
    pub fn action_geometry(&self, action: &ActionRef) -> Option<Rect> {
        let idx = self.actions.iter().position(|a| Rc::ptr_eq(a, action))?;
        self.item_rects().get(idx).copied()
    }

    /// Action whose item contains `pos` (menu coordinates).
    pub fn action_at(&self, pos: Point) -> Option<ActionRef> {
        self.index_at(pos)
            .and_then(|i| self.actions.get(i).cloned())
    }

    fn item_height(&self) -> i32 {
        if let Some(s) = self.effective_style() {
            let h = FontMetrics::from_font(&s.row_font).height.max(FontMetrics::from_font(&s.font).height).ceil() as i32;
            return h + s.item_padding[0] + s.item_padding[2];
        }
        let metrics = FontMetrics::from_font(&self.font);
        (metrics.height.ceil() as i32 + 8).max(24)
    }

    fn item_rects(&self) -> Vec<Rect> {
        let width = self.base.geometry().width - 2 * FRAME;
        let item_h = self.item_height();
        let mut y = self.top_inset();
        self.actions
            .iter()
            .map(|action| {
                let a = action.borrow();
                if !a.is_visible() {
                    return Rect::new(FRAME, y, 0, 0);
                }
                let h = if a.is_separator() {
                    self.separator_height()
                } else {
                    item_h
                };
                let rect = Rect::new(FRAME, y, width, h);
                y += h;
                rect
            })
            .collect()
    }

    fn index_at(&self, pos: Point) -> Option<usize> {
        self.item_rects().iter().position(|r| r.contains(pos))
    }

    fn is_selectable(&self, idx: usize) -> bool {
        self.actions.get(idx).is_some_and(|a| {
            let a = a.borrow();
            a.is_visible() && !a.is_separator() && a.is_enabled()
        })
    }

    fn has_submenu(&self, idx: usize) -> bool {
        self.actions
            .get(idx)
            .is_some_and(|a| a.borrow().menu().is_some())
    }

    fn set_active_index(&mut self, idx: Option<usize>) {
        if self.active == idx {
            return;
        }
        self.active = idx;
        if let Some(action) = idx.and_then(|i| self.actions.get(i).cloned()) {
            Action::hover(&action);
            self.hovered.emit(&action);
        }
        self.update();
    }

    fn step_active(&mut self, forward: bool) {
        let n = self.actions.len();
        if n == 0 {
            return;
        }
        let start = self.active.unwrap_or(if forward { n - 1 } else { 0 });
        for step in 1..=n {
            let i = if forward {
                (start + step) % n
            } else {
                (start + n - step) % n
            };
            if self.is_selectable(i) {
                self.set_active_index(Some(i));
                return;
            }
        }
    }

    /// Highlights the first selectable item (used when opening via keyboard).
    pub(crate) fn select_first(&mut self) {
        if let Some(i) = (0..self.actions.len()).find(|&i| self.is_selectable(i)) {
            self.set_active_index(Some(i));
        }
    }

    fn select_last(&mut self) {
        if let Some(i) = (0..self.actions.len())
            .rev()
            .find(|&i| self.is_selectable(i))
        {
            self.set_active_index(Some(i));
        }
    }

    /// Shows the menu at `geometry` without emitting `about_to_show` (callers do).
    pub(crate) fn show_with_geometry(&mut self, geometry: Rect) {
        self.close_submenu();
        self.active = None;
        self.base.set_geometry(geometry);
        self.base.set_visible(true);
        self.update();
    }

    fn open_submenu_at(&mut self, idx: usize, select_first: bool) {
        let Some(action) = self.actions.get(idx).cloned() else {
            return;
        };
        let (submenu, enabled) = {
            let a = action.borrow();
            (a.menu(), a.is_enabled())
        };
        let Some(submenu) = submenu else {
            return;
        };
        if let Some((open_idx, open)) = &self.open_submenu {
            if *open_idx == idx {
                if select_first {
                    open.borrow_mut().select_first();
                }
                return;
            }
        }
        self.close_submenu();
        if !enabled {
            return;
        }
        let item = self.item_rects()[idx];
        let geom = self.base.geometry();
        let local_bounds = self.popup_bounds.map(|b| b.translated(-geom.x, -geom.y));
        // `aboutToShow` runs with the sub-menu's `RefCell` free: the Qt idiom is to fill the menu
        // from this signal, which needs the menu itself. The geometry below sees what it added.
        let about_to_show = match submenu.try_borrow() {
            Ok(sub) => sub.about_to_show.clone(),
            Err(_) => return,
        };
        about_to_show.emit(&());
        {
            let mut sub = submenu.borrow_mut();
            sub.background_color = self.background_color;
            sub.border_color = self.border_color;
            sub.text_color = self.text_color;
            sub.disabled_text_color = self.disabled_text_color;
            sub.highlight_color = self.highlight_color;
            sub.highlight_text_color = self.highlight_text_color;
            sub.separator_color = self.separator_color;
            sub.font = self.font.clone();
            sub.style = self.style.clone();
            sub.set_parent_widget(self.base.parent_widget());
            sub.popup_bounds = local_bounds;
            let size = sub.size_hint();
            let mut x = geom.width - SUBMENU_OVERLAP;
            let mut y = item.y - self.top_inset();
            if let Some(b) = local_bounds {
                if x + size.width > b.right() {
                    x = (SUBMENU_OVERLAP - size.width).max(b.x);
                }
                if y + size.height > b.bottom() {
                    y = (b.bottom() - size.height).max(b.y);
                }
                y = y.max(b.y);
            }
            sub.popup_bounds = local_bounds.map(|b| b.translated(-x, -y));
            sub.show_with_geometry(Rect::new(x, y, size.width, size.height));
            if select_first {
                sub.select_first();
            }
        }
        self.open_submenu = Some((idx, submenu));
        self.update();
    }

    fn close_submenu(&mut self) {
        if let Some((_, submenu)) = self.open_submenu.take() {
            hide_shared(&submenu);
            self.update();
        }
    }

    /// Chooses the item at `idx`: opens its sub-menu or hides the menu and reports activation.
    fn activate_index(&mut self, idx: usize) -> MenuOutcome {
        let Some(action) = self.actions.get(idx).cloned() else {
            return MenuOutcome::Handled;
        };
        let (enabled, has_menu, separator) = {
            let a = action.borrow();
            (a.is_enabled(), a.menu().is_some(), a.is_separator())
        };
        if !enabled || separator {
            return MenuOutcome::Handled;
        }
        if has_menu {
            self.set_active_index(Some(idx));
            self.open_submenu_at(idx, true);
            return MenuOutcome::Handled;
        }
        self.hide_menu();
        self.last_triggered = Some(action.clone());
        MenuOutcome::Activate(action, vec![self.triggered.clone()])
    }

    /// Converts the outcome reported by the open sub-menu into this menu's outcome.
    fn outcome_from_child(&mut self, outcome: MenuOutcome) -> MenuOutcome {
        match outcome {
            MenuOutcome::Ignored => MenuOutcome::Ignored,
            MenuOutcome::Handled => {
                self.update();
                MenuOutcome::Handled
            }
            MenuOutcome::Closed | MenuOutcome::NavigateLeft => {
                self.close_submenu();
                MenuOutcome::Handled
            }
            MenuOutcome::NavigateRight => MenuOutcome::NavigateRight,
            MenuOutcome::Activate(action, mut signals) => {
                self.open_submenu = None;
                self.hide_menu();
                self.last_triggered = Some(action.clone());
                signals.push(self.triggered.clone());
                MenuOutcome::Activate(action, signals)
            }
        }
    }

    /// Returns true if `pos` (menu coordinates) is inside this menu or an open sub-menu.
    pub(crate) fn covers(&self, pos: Point) -> bool {
        if !self.base.is_visible() {
            return false;
        }
        let g = self.base.geometry();
        if Rect::new(0, 0, g.width, g.height).contains(pos) {
            return true;
        }
        self.open_submenu.as_ref().is_some_and(|(_, submenu)| {
            submenu.try_borrow().is_ok_and(|s| {
                let sg = s.base.geometry();
                s.covers(Point::new(pos.x - sg.x, pos.y - sg.y))
            })
        })
    }

    /// Bounding rectangle of this menu and its open sub-menus, in menu coordinates.
    pub fn covered_rect(&self) -> Rect {
        let g = self.base.geometry();
        let mut rect = Rect::new(0, 0, g.width, g.height);
        if let Some((_, submenu)) = &self.open_submenu {
            if let Ok(s) = submenu.try_borrow() {
                let sg = s.base.geometry();
                rect = rect.united(&s.covered_rect().translated(sg.x, sg.y));
            }
        }
        rect
    }
    /// Visual hover signature tracking the active item and open submenu at every hierarchy level.
    pub fn hover_signature(&self) -> Vec<(Option<usize>, Option<usize>)> {
        let mut sig = vec![(self.active, self.open_submenu.as_ref().map(|(i, _)| *i))];
        let mut curr = self.open_submenu.as_ref().map(|(_, s)| s.clone());
        while let Some(sub_rc) = curr {
            if let Ok(s) = sub_rc.try_borrow() {
                sig.push((s.active, s.open_submenu.as_ref().map(|(i, _)| *i)));
                curr = s.open_submenu.as_ref().map(|(_, next)| next.clone());
            } else {
                break;
            }
        }
        sig
    }

    /// Handles mouse motion at `pos` in menu coordinates.
    pub fn handle_mouse_move_at(&mut self, pos: Point) {
        self.handle_mouse_move(pos);
    }

    fn route_to_submenu<F>(&mut self, pos: Point, f: F) -> Option<MenuOutcome>
    where
        F: FnOnce(&mut Menu, Point) -> MenuOutcome,
    {
        let (_, submenu) = self.open_submenu.clone()?;
        let origin = {
            let s = submenu.borrow();
            let sg = s.base.geometry();
            if !s.covers(Point::new(pos.x - sg.x, pos.y - sg.y)) {
                return None;
            }
            sg
        };
        let outcome = f(
            &mut submenu.borrow_mut(),
            Point::new(pos.x - origin.x, pos.y - origin.y),
        );
        Some(self.outcome_from_child(outcome))
    }

    /// Keyboard handling (`QMenu::keyPressEvent`); the deepest open sub-menu gets the key first.
    pub(crate) fn handle_key(&mut self, key: u32, modifiers: u32) -> MenuOutcome {
        let key = crate::key_sequence_edit::normalize_key(key, modifiers).0;
        let modifiers = keys::normalize_modifiers(modifiers);
        self.handle_normalized_key(key, modifiers)
    }

    /// `handle_key` for a key already run through `normalize_key`; recursion must not normalize
    /// again (the apostrophe, 0x27, would become VK_RIGHT).
    fn handle_normalized_key(&mut self, key: u32, modifiers: u32) -> MenuOutcome {
        if let Some((_, submenu)) = self.open_submenu.clone() {
            let outcome = submenu.borrow_mut().handle_normalized_key(key, modifiers);
            return self.outcome_from_child(outcome);
        }
        match key {
            keys::UP => {
                self.step_active(false);
                MenuOutcome::Handled
            }
            keys::DOWN => {
                self.step_active(true);
                MenuOutcome::Handled
            }
            keys::HOME | keys::PAGE_UP => {
                self.select_first();
                MenuOutcome::Handled
            }
            keys::END | keys::PAGE_DOWN => {
                self.select_last();
                MenuOutcome::Handled
            }
            keys::LEFT => MenuOutcome::NavigateLeft,
            keys::RIGHT => match self.active {
                Some(idx) if self.has_submenu(idx) && self.is_selectable(idx) => {
                    self.open_submenu_at(idx, true);
                    MenuOutcome::Handled
                }
                _ => MenuOutcome::NavigateRight,
            },
            keys::RETURN | keys::ENTER | keys::SPACE => match self.active {
                Some(idx) => self.activate_index(idx),
                None => MenuOutcome::Handled,
            },
            keys::ESCAPE => {
                self.hide_menu();
                MenuOutcome::Closed
            }
            _ => self.handle_mnemonic(key, modifiers),
        }
    }

    /// Mnemonic handling: a unique match is activated, clashing matches are cycled.
    fn handle_mnemonic(&mut self, key: u32, modifiers: u32) -> MenuOutcome {
        let allowed = modifiers & !(keys::MOD_ALT | keys::MOD_SHIFT) == 0;
        let Some(c) = keys::key_char(key).filter(|_| allowed) else {
            return MenuOutcome::Ignored;
        };
        let matches: Vec<usize> = (0..self.actions.len())
            .filter(|&i| {
                let a = self.actions[i].borrow();
                a.is_visible() && !a.is_separator() && a.mnemonic() == Some(c)
            })
            .collect();
        let Some(&first) = matches.first() else {
            return MenuOutcome::Ignored;
        };
        let next = match self.active {
            Some(current) if matches.len() > 1 => matches
                .iter()
                .copied()
                .find(|&i| i > current)
                .unwrap_or(first),
            _ => first,
        };
        if matches.len() == 1 {
            return self.activate_index(next);
        }
        self.set_active_index(Some(next));
        MenuOutcome::Handled
    }

    /// True when the pointer moved from `from` to `pos` heading for the open submenu, i.e. `pos`
    /// lies inside the wedge spanned by `from` and the submenu's near edge.
    ///
    /// Mirrors Qt's `QMenuSloppyState` (qmenu.cpp): the submenu is not abandoned while the
    /// pointer crosses neighbouring items on its way to it. The decision depends on the direction
    /// of travel, so moving straight along the parent menu, or away from the submenu, selects the
    /// item under the pointer as usual (a plain bounding rectangle would swallow every row that
    /// lies within the submenu's vertical extent).
    fn is_moving_towards_submenu(&self, from: Point, pos: Point) -> bool {
        let Some((idx, submenu)) = &self.open_submenu else {
            return false;
        };
        let Ok(sub) = submenu.try_borrow() else {
            return false;
        };
        let rects = self.item_rects();
        let Some(&item) = rects.get(*idx) else {
            return false;
        };
        let sg = sub.base.geometry();

        // `side` is +1 when the submenu opens to the right, -1 when it opens to the left; all
        // x distances below are measured towards the submenu.
        // Qt uses the inclusive QRect::right() (last pixel), i.e. exclusive right() - 1 (qmenu.cpp:2523-2535).
        let (side, edge_x) = if sg.x >= (item.right() - 1) - SUBMENU_OVERLAP - 4 {
            (1i64, sg.x)
        } else {
            (-1i64, sg.x + sg.width)
        };
        let to_edge = side * (edge_x - from.x) as i64;
        let dx = side * (pos.x - from.x) as i64;
        if to_edge <= 0 || dx <= 0 {
            return false;
        }
        let dy = (pos.y - from.y) as i64;
        let top = (sg.y - from.y) as i64;
        let bottom = (sg.y + sg.height - from.y) as i64;
        // cross((to_edge, top), (dx, dy)) >= 0 && cross((dx, dy), (to_edge, bottom)) >= 0
        to_edge * dy - top * dx >= 0 && dx * bottom - dy * to_edge >= 0
    }

    pub(crate) fn handle_mouse_move(&mut self, pos: Point) -> MenuOutcome {
        if let Some(outcome) = self.route_to_submenu(pos, |s, p| s.handle_mouse_move(p)) {
            self.last_mouse_pos = None;
            return outcome;
        }
        // Parity with Qt QMenu::mouseMoveEvent (qmenu.cpp:3511):
        // When mouse is inside this menu, clear active hover selection on open submenus
        if let Some((_, submenu)) = &self.open_submenu {
            if let Ok(mut sub) = submenu.try_borrow_mut() {
                sub.set_active_index(None);
            }
        }
        // While the pointer travels diagonally towards the open submenu, keep the submenu and
        // the active item (mirrors Qt's sloppyState). Any other move selects the item under it.
        let from = self.last_mouse_pos.replace(pos);
        if from.is_some_and(|from| self.is_moving_towards_submenu(from, pos)) {
            return MenuOutcome::Handled;
        }
        if let Some(idx) = self.index_at(pos) {
            if self.is_selectable(idx) && self.active != Some(idx) {
                self.set_active_index(Some(idx));
                if self.has_submenu(idx) {
                    self.open_submenu_at(idx, false);
                } else {
                    self.close_submenu();
                }
            }
            return MenuOutcome::Handled;
        }
        if self.covers(pos) {
            MenuOutcome::Handled
        } else {
            MenuOutcome::Ignored
        }
    }

    pub(crate) fn handle_mouse_press(&mut self, pos: Point, _button: u32) -> MenuOutcome {
        if let Some(outcome) = self.route_to_submenu(pos, |s, p| s.handle_mouse_press(p, _button)) {
            return outcome;
        }
        if !self.covers(pos) {
            self.hide_menu();
            return MenuOutcome::Closed;
        }
        if let Some(idx) = self.index_at(pos) {
            if self.is_selectable(idx) && self.has_submenu(idx) {
                if self.open_submenu.as_ref().is_some_and(|(i, _)| *i == idx) {
                    self.close_submenu();
                } else {
                    self.set_active_index(Some(idx));
                    self.open_submenu_at(idx, false);
                }
            }
        }
        MenuOutcome::Handled
    }

    pub(crate) fn handle_mouse_release(&mut self, pos: Point, _button: u32) -> MenuOutcome {
        if let Some(outcome) = self.route_to_submenu(pos, |s, p| s.handle_mouse_release(p, _button))
        {
            return outcome;
        }
        if let Some(idx) = self.index_at(pos) {
            if self.is_selectable(idx) && !self.has_submenu(idx) {
                return self.activate_index(idx);
            }
            return MenuOutcome::Handled;
        }
        if self.covers(pos) {
            MenuOutcome::Handled
        } else {
            MenuOutcome::Ignored
        }
    }

    pub(crate) fn handle_leave(&mut self) {
        self.last_mouse_pos = None;
        if self.open_submenu.is_none() {
            self.set_active_index(None);
        }
    }

    fn finish_top_level(&mut self, outcome: MenuOutcome) -> bool {
        match outcome {
            MenuOutcome::Ignored => false,
            MenuOutcome::Activate(action, signals) => {
                MenuOutcome::fire(&action, &signals);
                true
            }
            MenuOutcome::Handled
            | MenuOutcome::Closed
            | MenuOutcome::NavigateLeft
            | MenuOutcome::NavigateRight => true,
        }
    }

    fn paint_item(&self, painter: &mut Painter, action: &Action, rect: Rect, active: bool) {
        let style = self.effective_style();
        let colors = style.clone().unwrap_or_else(|| self.hard_style());
        let font = colors.font.clone();
        let metrics = FontMetrics::from_font(&font);
        let width = self.base.geometry().width;
        if action.is_separator() {
            let (x0, x1, line_y) = match &style {
                Some(s) => (
                    (FRAME + s.separator_margin[1]) as f32,
                    (width - FRAME - s.separator_margin[1]) as f32,
                    (rect.y + s.separator_margin[0]) as f32 + 0.5,
                ),
                None => (
                    CHECK_COLUMN as f32 - 4.0,
                    (width - RIGHT_PADDING) as f32,
                    rect.y as f32 + rect.height as f32 / 2.0,
                ),
            };
            painter.set_render_hint(RenderHint::Antialiasing, false);
            painter.set_pen(Pen::new(colors.separator, 1.0));
            painter.draw_line(PointF::new(x0, line_y), PointF::new(x1, line_y));
            return;
        }
        let enabled = action.is_enabled();
        if active && enabled {
            painter.set_brush(Brush::Color(colors.hover_background));
            painter.set_pen(None);
            // Win11-style rounded highlight (no style sheet) is anti-aliased; the square
            // style-sheet highlight is not.
            painter.set_render_hint(RenderHint::Antialiasing, style.is_none());
            let (hl, radius) = if style.is_some() {
                (rect, 0.0)
            } else {
                (Rect::new(rect.x + 3, rect.y, rect.width - 6, rect.height), 4.0)
            };
            painter.draw_rounded_rect(
                RectF::new(hl.x as f32, hl.y as f32, hl.width as f32, hl.height as f32),
                radius,
                radius,
            );
        }
        let color = if !enabled {
            colors.disabled_text
        } else if active {
            colors.hover_text
        } else {
            colors.text
        };
        let mid_y = rect.y as f32 + rect.height as f32 / 2.0;
        let text_x = self.text_x();

        if action.is_checkable() && action.is_checked() {
            if style.is_some() && !action.is_exclusive_in_group() {
                // `QStyleSheetStyle` draws the check through the base style's
                // `PE_IndicatorMenuCheckMark`, in a box of `check_indicator()` px at the item's
                // left edge (`positionRect`: AlignLeft | AlignVCenter, padding origin),
                // coloured like the item text.
                let size = check_indicator();
                let box_y = rect.y + (rect.height - size) / 2;
                let anti_aliased = native_style() == ferrokit_platform::NativeStyle::Windows11;
                paint_common_check_mark(
                    painter,
                    Rect::new(FRAME, box_y, size, size),
                    color,
                    anti_aliased,
                );
            } else {
                let check_color = if active {
                    colors.hover_text
                } else {
                    Color::from_rgba8(56, 189, 248, 255)
                };
                if action.is_exclusive_in_group() {
                    painter.set_brush(Brush::Color(check_color));
                    painter.set_pen(None);
                    // `qfusionstyle.cpp:1395, 1404`: radio indicator is anti-aliased.
                    painter.set_render_hint(RenderHint::Antialiasing, true);
                    painter.draw_ellipse(RectF::new(10.0, mid_y - 4.0, 8.0, 8.0));
                } else {
                    painter.set_render_hint(RenderHint::Antialiasing, true);
                    painter.set_pen(Pen::new(check_color, 2.0).with_cap(ferrokit_gui::tiny_skia::LineCap::Round).with_join(ferrokit_gui::tiny_skia::LineJoin::Round));
                    painter.draw_line(PointF::new(9.0, mid_y), PointF::new(12.5, mid_y + 3.5));
                    painter.draw_line(
                        PointF::new(12.5, mid_y + 3.5),
                        PointF::new(19.0, mid_y - 4.0),
                    );
                }
            }
        } else if !action.icon().is_null() {
            let mode = if enabled {
                IconMode::Normal
            } else {
                IconMode::Disabled
            };
            let pixmap =
                action
                    .icon()
                    .pixmap(Size::new(ICON_SIZE, ICON_SIZE), mode, IconState::Off);
            painter.set_render_hint(RenderHint::Antialiasing, false);
            painter.draw_pixmap(
                RectF::new(
                    6.0,
                    mid_y - ICON_SIZE as f32 / 2.0,
                    ICON_SIZE as f32,
                    ICON_SIZE as f32,
                ),
                &pixmap,
                None,
            );
        }

        let text = action.display_text();
        let baseline = mid_y - metrics.height / 2.0 + metrics.ascent;
        painter.draw_text_colored(
            PointF::new(text_x, baseline),
            &text,
            &font,
            color,
        );
        if let Some(offset) = keys::mnemonic_offset(action.text()) {
            let prefix_w = metrics.horizontal_advance(&text[..offset], &font);
            let ch: String = text[offset..].chars().take(1).collect();
            let ch_w = metrics.horizontal_advance(&ch, &font);
            let underline_y = baseline + 2.0;
            painter.set_render_hint(RenderHint::Antialiasing, false);
            painter.set_pen(Pen::new(color, 1.0));
            painter.draw_line(
                PointF::new(text_x + prefix_w, underline_y),
                PointF::new(text_x + prefix_w + ch_w, underline_y),
            );
        }

        let shortcut = action.shortcut();
        if !shortcut.is_empty() {
            let sc_text = shortcut.to_string();
            let sc_w = metrics.horizontal_advance(&sc_text, &font);
            let x = (width - ARROW_COLUMN) as f32 - sc_w;
            painter.draw_text_colored(PointF::new(x, baseline), &sc_text, &font, color);
        }

        if action.menu().is_some() {
            if style.is_some() {
                paint_submenu_arrow(painter, rect, color);
            } else {
                let ax = (width - 14) as f32;
                painter.set_pen(Pen::new(color, 1.5).with_cap(ferrokit_gui::tiny_skia::LineCap::Round).with_join(ferrokit_gui::tiny_skia::LineJoin::Round));
                painter.set_render_hint(RenderHint::Antialiasing, true);
                painter.draw_line(PointF::new(ax, mid_y - 4.0), PointF::new(ax + 4.0, mid_y));
                painter.draw_line(PointF::new(ax + 4.0, mid_y), PointF::new(ax, mid_y + 4.0));
            }
        }
    }
}

impl Default for Menu {
    fn default() -> Self {
        Self::new("")
    }
}

impl QObject for Menu {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        // `QWidget::changeEvent(EnabledChange)` repaints; the menu's dirty area is its own.
        if matches!(event.kind, EventKind::EnabledChange) {
            self.update();
            return false;
        }
        if !self.base.is_visible() {
            return false;
        }
        let outcome = match &event.kind {
            EventKind::KeyPress { key, modifiers, .. } => self.handle_key(*key, *modifiers),
            EventKind::MouseMove { x, y } => self.handle_mouse_move(Point::new(*x, *y)),
            EventKind::MouseButtonPress { x, y, button } => {
                self.handle_mouse_press(Point::new(*x, *y), *button)
            }
            EventKind::MouseButtonRelease { x, y, button } => {
                self.handle_mouse_release(Point::new(*x, *y), *button)
            }
            EventKind::Leave => {
                self.handle_leave();
                MenuOutcome::Handled
            }
            _ => return false,
        };
        self.finish_top_level(outcome)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Widget for Menu {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
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

    fn size_hint(&self) -> Size {
        let measure = self.measure_font();
        let metrics = FontMetrics::from_font(&measure);
        let item_h = self.item_height();
        let mut height = 2 * self.top_inset();
        let mut max_text = 0i32;
        let mut max_shortcut = 0i32;
        for action in &self.actions {
            let a = action.borrow();
            if !a.is_visible() {
                continue;
            }
            if a.is_separator() {
                height += self.separator_height();
                continue;
            }
            height += item_h;
            let text_w = metrics
                .horizontal_advance_exact(&a.display_text(), &measure)
                .ceil() as i32;
            max_text = max_text.max(text_w);
            if !a.shortcut().is_empty() {
                let sc_w = metrics
                    .horizontal_advance_exact(&a.shortcut().to_string(), &measure)
                    .ceil() as i32;
                max_shortcut = max_shortcut.max(sc_w);
            }
        }
        let shortcut_w = if max_shortcut > 0 {
            SHORTCUT_GAP + max_shortcut
        } else {
            0
        };
        let width = self.width_for(max_text, shortcut_w);
        Size::new(width, height)
    }

    fn minimum_size_hint(&self) -> Size {
        self.size_hint()
    }

    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }

    fn set_visible(&self, visible: bool) {
        if visible == self.base.is_visible() {
            return;
        }
        let announced = self.announced.replace(false);
        if visible {
            if !announced {
                self.about_to_show.emit(&());
            }
            let g = self.base.geometry();
            let size = self.size_hint();
            self.base.set_geometry(Rect::new(g.x, g.y, size.width, size.height));
            self.base.set_visible(true);
        } else {
            if !announced {
                self.about_to_hide.emit(&());
            }
            self.base.set_visible(false);
        }
        self.update();
    }

    fn announce_visibility(&self, visible: bool) -> Option<Signal<()>> {
        if visible == self.base.is_visible() {
            return None;
        }
        self.announced.set(true);
        Some(if visible { self.about_to_show.clone() } else { self.about_to_hide.clone() })
    }

    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }

    fn update(&self) {
        let covered = self.covered_rect();
        let old_covered = self.last_covered.get();
        self.base.dirty.set(Some(covered.united(&old_covered)));
        self.last_covered.set(covered);
        let target = self.base.window_id().unwrap_or(self.base.object_data.id());
        let target_thread = ferrokit_core::object::query_object_thread(target)
            .unwrap_or(self.base.object_data.thread_id());
        ferrokit_core::event_loop::post_event_to_thread(
            target_thread,
            target,
            Event::new(EventKind::UpdateRequest),
        );
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
        Vec::new()
    }

    fn add_child(&mut self, _child: WidgetRef) {}

    fn remove_child(&mut self, _child_id: ObjectId) {}

    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }

    fn set_focus_policy(&self, policy: FocusPolicy) {
        self.base.set_focus_policy(policy);
    }

    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }

    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
    }

    fn mouse_press_event(&mut self, pos: Point, button: u32, _modifiers: u32) {
        let outcome = self.handle_mouse_press(pos, button);
        self.finish_top_level(outcome);
    }

    fn mouse_release_event(&mut self, pos: Point, button: u32, _modifiers: u32) {
        let outcome = self.handle_mouse_release(pos, button);
        self.finish_top_level(outcome);
    }

    fn mouse_move_event(&mut self, pos: Point) {
        let outcome = self.handle_mouse_move(pos);
        self.finish_top_level(outcome);
    }

    fn leave_event(&mut self) {
        self.handle_leave();
    }

    fn key_press_event(&mut self, key: u32, modifiers: u32, _is_repeat: bool) {
        if self.base.is_visible() {
            let outcome = self.handle_key(key, modifiers);
            self.finish_top_level(outcome);
        }
    }

    fn paint_event(&mut self, painter: &mut Painter) {
        if !self.base.is_visible() {
            return;
        }
        let g = self.base.geometry();
        let s = self.effective_style().unwrap_or_else(|| self.hard_style());
        painter.set_brush(Brush::Color(s.background));
        painter.set_pen(Pen::new(s.border, 1.0));
        painter.set_render_hint(RenderHint::Antialiasing, true);
        painter.draw_rounded_rect(
            RectF::new(
                0.5,
                0.5,
                g.width as f32 - 1.0,
                g.height as f32 - 1.0,
            ),
            s.radius,
            s.radius,
        );

        let rects = self.item_rects();
        for (i, action) in self.actions.iter().enumerate() {
            let rect = rects[i];
            if rect.width <= 0 || rect.height <= 0 {
                continue;
            }
            let a = action.borrow();
            self.paint_item(painter, &a, rect, self.active == Some(i));
        }

        if let Some((_, submenu)) = &self.open_submenu {
            if let Ok(mut sub) = submenu.try_borrow_mut() {
                let sg = sub.base.geometry();
                painter.save();
                // A sub-menu is its own native window in Qt, so it starts on a whole pixel.
                let dpr = painter.device_pixel_ratio();
                painter.translate_device(
                    (sg.x as f32 * dpr).round() as i32,
                    (sg.y as f32 * dpr).round() as i32,
                );
                sub.paint_event(painter);
                painter.restore();
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The base style's `PE_IndicatorMenuCheckMark`: `QCommonStyle`'s 7 px check built from 1 px wide,
/// 2 px long vertical strokes, centred in `rect` and shifted 1 px right. `QWindows11Style` leaves
/// its painter anti-aliased (square caps, so each stroke is 3 rows tall and straddles two
/// columns); `windowsvista` and `Fusion` draw it with plain pixels (column `x`, rows `y..=y+2`).
/// `macos` instead draws a Core Text glyph (`kCTFontUIFontMenuItemMark`), which is not
/// reproduced; this check stands in for it there.
fn paint_common_check_mark(painter: &mut Painter, rect: Rect, color: Color, anti_aliased: bool) {
    painter.set_render_hint(RenderHint::Antialiasing, anti_aliased);
    let mark = rect.width.min(7);
    let pos_x = rect.x + (rect.width - mark) / 2 + 1;
    let pos_y = rect.y + (rect.height - mark) / 2;

    let mut strokes = Vec::with_capacity(mark as usize);
    let (mut x, mut y) = (pos_x, 3 + pos_y);
    let mut i = 0;
    while i < mark / 2 {
        strokes.push((x, y));
        x += 1;
        y += 1;
        i += 1;
    }
    y -= 2;
    while i < mark {
        strokes.push((x, y));
        x += 1;
        y -= 1;
        i += 1;
    }

    if anti_aliased {
        painter.set_pen(Pen::new(color, 1.0).with_cap(ferrokit_gui::tiny_skia::LineCap::Square));
        for (x, y) in strokes {
            painter.draw_line(
                PointF::new(x as f32, y as f32),
                PointF::new(x as f32, (y + 2) as f32),
            );
        }
    } else {
        for (x, y) in strokes {
            painter.fill_rect(RectF::new(x as f32, y as f32, 1.0, 3.0), color);
        }
    }
}

/// `QMacStyle` `PE_IndicatorArrowRight`: a stroked chevron (round caps and joins, anti-aliased) of
/// half-size `min(w, h) / 2`, the "down" chevron rotated by -90 degrees about the square's centre
/// shifted by (1, 2) px (`qmacstyle_mac.mm`). Qt strokes it with the palette's text colour.
fn paint_macos_arrow(painter: &mut Painter, rect: Rect, color: Color) {
    painter.set_render_hint(RenderHint::Antialiasing, true);
    let half = 0.5 * rect.width.min(rect.height) as f32;
    let pen_width = (half / 3.0).max(1.25);
    let cx = (rect.x + (rect.width - 1) / 2 + 1) as f32; // QRect::center().x() + 1
    let cy = (rect.y + (rect.height - 1) / 2 + 2) as f32; // QRect::center().y() + 2
    painter.set_pen(Pen::new(color, pen_width).with_cap(ferrokit_gui::tiny_skia::LineCap::Round).with_join(ferrokit_gui::tiny_skia::LineJoin::Round));
    painter.draw_polyline(&[
        PointF::new(cx - half / 2.0, cy + half),
        PointF::new(cx + half / 2.0, cy),
        PointF::new(cx - half / 2.0, cy - half),
    ]);
}


/// Submenu arrow of a style-sheet-driven menu item, as `QStyleSheetStyle` draws it
/// (`CE_MenuItem`): without a `::right-arrow` rule it hands the base style a square of half the
/// row height, centred that far left of the item's right edge, and the base style draws the
/// arrow, so the result differs by platform style:
/// - `windows11`: the Fluent chevron glyph (`QWindows11Style`, `PE_IndicatorArrowRight`);
/// - `windowsvista`: `QCommonStyle`'s pixel triangle;
/// - `macos`: a stroked chevron (`QMacStyle`, from its source; not run on macOS);
/// - `Fusion` (Linux, others): `qt_fusion_draw_arrow`'s anti-aliased triangle. Qt tints it with
///   the palette's `windowText` at alpha 160; without a palette the item colour stands in.
fn paint_submenu_arrow(painter: &mut Painter, item: Rect, color: Color) {
    use ferrokit_platform::NativeStyle;

    let dim = item.height / 2;
    let right = item.x + item.width - 1; // QRect::right()
    let center_y = item.y + (item.height - 1) / 2; // QRect::center().y()
    // QRect::moveCenter on a dim x dim rect.
    let (left, top) = (right - dim - (dim - 1) / 2, center_y - (dim - 1) / 2);
    let square = Rect::new(left, top, dim, dim);

    match ferrokit_platform::platform().theme().native_style() {
        NativeStyle::Windows11 => paint_fluent_chevron(painter, square, color),
        NativeStyle::WindowsVista => paint_common_arrow(painter, square, color),
        NativeStyle::Macintosh => paint_macos_arrow(painter, square, color),
        NativeStyle::Fusion => paint_fusion_arrow(painter, square, color),
    }
}

/// `QWindows11Style`: the `ChevronRightMed` glyph of the icon font, centred in `rect`.
fn paint_fluent_chevron(painter: &mut Painter, rect: Rect, color: Color) {
    const CHEVRON_RIGHT_MED: &str = "\u{E974}";
    // `assetFont` is a `QFont(QStringList)` with no size, so it takes the application font's
    // 9 pt = 12 px (measured: the glyph is 8 px tall, 5 px wide).
    let font = Font::new("Segoe Fluent Icons, Segoe MDL2 Assets", 12.0);
    let metrics = FontMetrics::from_font(&font);
    let advance = metrics.horizontal_advance_exact(CHEVRON_RIGHT_MED, &font);
    // Qt's DirectWrite engine has no sub-pixel positioning, so the origin is whole pixels.
    let x = (rect.x as f32 + (rect.width as f32 - advance) / 2.0).round();
    let y = (rect.y as f32 + (rect.height as f32 - metrics.height) / 2.0 + metrics.ascent).round();
    painter.draw_text_colored(PointF::new(x, y), CHEVRON_RIGHT_MED, &font, color);
}

/// `QCommonStyle::drawPrimitive(PE_IndicatorArrowRight)`: an un-anti-aliased triangle of
/// `size - 3` by `(size - 3) / 2` px, centred in the square.
fn paint_common_arrow(painter: &mut Painter, rect: Rect, color: Color) {
    // `QCommonStyle` triangle: un-anti-aliased pixel rows.
    painter.set_render_hint(RenderHint::Antialiasing, false);
    let size = rect.width.min(rect.height);
    let long = size - 3;
    let (width, height) = (long / 2, long); // transposed for a right arrow
    let (ox, oy) = (rect.x + (size - width) / 2, rect.y + (size - height) / 2);
    let half = (height / 2).max(1);
    for y in 0..=height {
        let reach = (width * (half - (y - half).abs()) + half / 2) / half;
        painter.fill_rect(
            RectF::new(ox as f32, (oy + y) as f32, (reach + 1) as f32, 1.0),
            color,
        );
    }
}

/// `qt_fusion_draw_arrow`: an anti-aliased triangle 4 x 8 px (for a 13 px square).
fn paint_fusion_arrow(painter: &mut Painter, rect: Rect, color: Color) {
    use ferrokit_gui::tiny_skia::PathBuilder;
    const ARROW_WIDTH: i32 = 14; // dpiScaled(14) at 96 dpi
    const ARROW_HEIGHT: i32 = 8;
    let size = ARROW_HEIGHT.min(ARROW_WIDTH).min(rect.width.min(rect.height));
    let (w, h) = ((ARROW_HEIGHT * size / ARROW_WIDTH) as f32, size as f32); // transposed
    let x = rect.x as f32 + (rect.width as f32 - w) / 2.0;
    let y = rect.y as f32 + (rect.height as f32 - h) / 2.0;
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x, y + h);
    pb.line_to(x + w, y + h / 2.0);
    pb.close();
    // `qt_fusion_draw_arrow` is anti-aliased (`qfusionstyle.cpp:392-405`).
    painter.set_render_hint(RenderHint::Antialiasing, true);
    if let Some(path) = pb.finish() {
        let mut tint = color;
        tint.set_alpha(160.0 / 255.0);
        painter.set_brush(Brush::Color(tint));
        painter.set_pen(None);
        painter.fill_path(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokit_gui::paint::pixmap::Pixmap;

    fn text() -> Color {
        Color::from_rgba8(226, 232, 240, 255)
    }

    fn canvas(w: u32, h: u32) -> Pixmap {
        let mut pixmap = Pixmap::new(w, h).unwrap();
        pixmap.fill(Color::from_rgba8(22, 25, 32, 255));
        pixmap
    }

    fn red(pixmap: &Pixmap, x: u32, y: u32) -> i32 {
        pixmap.pixel(x, y).unwrap().red() as i32
    }

    /// Red channel of Qt's own check mark (PySide6 6.11.2, a card-style sheet: text
    /// `#e2e8f0` over `#161920`), rows y = 4..=11 and columns x = 3..=15 of a 16 px indicator box
    /// whose top-left is (1, 0).
    const QT_CHECK: [[u8; 13]; 8] = [
        [22, 22, 22, 22, 22, 22, 22, 22, 73, 73, 22, 22, 22],
        [22, 22, 22, 22, 22, 22, 22, 73, 150, 124, 22, 22, 22],
        [22, 22, 73, 73, 22, 22, 73, 150, 175, 124, 22, 22, 22],
        [22, 22, 124, 149, 73, 73, 150, 175, 149, 73, 22, 22, 22],
        [22, 22, 124, 175, 149, 150, 175, 149, 73, 22, 22, 22, 22],
        [22, 22, 73, 150, 175, 175, 149, 73, 22, 22, 22, 22, 22],
        [22, 22, 22, 73, 150, 149, 73, 22, 22, 22, 22, 22, 22],
        [22, 22, 22, 22, 73, 73, 22, 22, 22, 22, 22, 22, 22],
    ];

    #[test]
    fn check_mark_matches_qt_pixels() {
        let mut pixmap = canvas(24, 16);
        {
            let mut painter = Painter::begin(&mut pixmap);
            paint_common_check_mark(&mut painter, Rect::new(FRAME, 0, 16, 16), text(), true);
        }
        // With the box top at y = 0 the first stroke's cap lands on row 4 (QT_CHECK row 0).
        for (r, row) in QT_CHECK.iter().enumerate() {
            for (c, &expected) in row.iter().enumerate() {
                let got = red(&pixmap, (c + 3) as u32, (r + 4) as u32);
                assert!(
                    (got - expected as i32).abs() <= 2,
                    "pixel ({}, {}): {got} vs Qt {expected}",
                    c + 3,
                    r + 4
                );
            }
        }
    }

    /// Plain-pixel check mark (`windowsvista`: indicator box 13 px at (1, 7); PySide6 6.11.2):
    /// the 226-valued pixels per row, rows 11..=17.
    const QT_PLAIN_CHECK: [(u32, &[u32]); 7] = [
        (11, &[11]),
        (12, &[10, 11]),
        (13, &[5, 9, 10, 11]),
        (14, &[5, 6, 8, 9, 10]),
        (15, &[5, 6, 7, 8, 9]),
        (16, &[6, 7, 8]),
        (17, &[7]),
    ];

    #[test]
    fn plain_check_mark_matches_qt_windowsvista() {
        let mut pixmap = canvas(24, 24);
        {
            let mut painter = Painter::begin(&mut pixmap);
            paint_common_check_mark(&mut painter, Rect::new(FRAME, 7, 13, 13), text(), false);
        }
        for y in 10..19u32 {
            let ink: &[u32] = QT_PLAIN_CHECK
                .iter()
                .find(|(row, _)| *row == y)
                .map_or(&[], |(_, xs)| xs);
            for x in 2..16u32 {
                let expected = if ink.contains(&x) { 226 } else { 22 };
                assert_eq!(red(&pixmap, x, y), expected, "pixel ({x}, {y})");
            }
        }
    }

    /// `windowsvista` (QCommonStyle) submenu arrow in a 13 px square whose top-left is (8, 7):
    /// per row, how many 226-valued pixels start at x = 12 (PySide6 6.11.2, rows 8..=18).
    const QT_COMMON_ARROW_WIDTHS: [u32; 11] = [1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1];

    #[test]
    fn common_arrow_matches_qt_windowsvista() {
        let mut pixmap = canvas(32, 24);
        {
            let mut painter = Painter::begin(&mut pixmap);
            paint_common_arrow(&mut painter, Rect::new(8, 7, 13, 13), text());
        }
        for (i, &width) in QT_COMMON_ARROW_WIDTHS.iter().enumerate() {
            let y = 8 + i as u32;
            for x in 8..24u32 {
                let expect_ink = x >= 12 && x < 12 + width;
                let got = red(&pixmap, x, y);
                assert_eq!(got == 226, expect_ink, "row {y} x {x}: red {got}");
                if !expect_ink {
                    assert_eq!(got, 22, "row {y} x {x} must stay background");
                }
            }
        }
    }

    /// Fusion submenu arrow (`qt_fusion_draw_arrow`) for the same square, red channel over the
    /// background, rows y = 9..=17, columns x = 12..=16 (PySide6 6.11.2, `Fusion`). tiny-skia's
    /// anti-aliasing covers diagonal edge pixels up to 17/255 more than Qt's scanline rasterizer.
    const QT_FUSION_ARROW: [[u8; 5]; 9] = [
        [38, 22, 22, 22, 22],
        [86, 86, 22, 22, 22],
        [86, 150, 86, 22, 22],
        [86, 150, 150, 86, 22],
        [86, 150, 150, 150, 55],
        [86, 150, 150, 86, 22],
        [86, 150, 86, 22, 22],
        [86, 86, 22, 22, 22],
        [38, 22, 22, 22, 22],
    ];

    #[test]
    fn fusion_arrow_matches_qt_fusion() {
        let mut pixmap = canvas(32, 24);
        {
            let mut painter = Painter::begin(&mut pixmap);
            paint_fusion_arrow(&mut painter, Rect::new(8, 7, 13, 13), text());
        }
        for (r, row) in QT_FUSION_ARROW.iter().enumerate() {
            for (c, &expected) in row.iter().enumerate() {
                let got = red(&pixmap, 12 + c as u32, 9 + r as u32);
                assert!(
                    (got - expected as i32).abs() <= 18,
                    "pixel ({}, {}): {got} vs Qt {expected}",
                    12 + c,
                    9 + r
                );
            }
        }
    }
}
