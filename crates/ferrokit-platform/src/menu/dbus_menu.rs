// SPDX-License-Identifier: LGPL-3.0-only
use super::{PlatformMenu, PlatformMenuItem};
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::Point;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum DBusMenuPropValue {
    String(String),
    Boolean(bool),
    Int(i32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DBusMenuLayoutNode {
    pub id: u32,
    pub properties: HashMap<String, DBusMenuPropValue>,
    pub children: Vec<DBusMenuLayoutNode>,
}

pub struct DBusMenuItem {
    id: u32,
    text: Mutex<String>,
    separator: bool,
    checkable: bool,
    checked: Mutex<bool>,
    enabled: Mutex<bool>,
    visible: Mutex<bool>,
    activated: Signal<()>,
    submenu: Mutex<Option<Box<dyn PlatformMenu>>>,
}

impl DBusMenuItem {
    pub fn new_action(id: u32, text: &str) -> Self {
        Self {
            id,
            text: Mutex::new(text.to_string()),
            separator: false,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(true),
            visible: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(None),
        }
    }

    pub fn new_checkable(id: u32, text: &str, checked: bool) -> Self {
        Self {
            id,
            text: Mutex::new(text.to_string()),
            separator: false,
            checkable: true,
            checked: Mutex::new(checked),
            enabled: Mutex::new(true),
            visible: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(None),
        }
    }

    pub fn new_separator() -> Self {
        Self {
            id: 0,
            text: Mutex::new(String::new()),
            separator: true,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(false),
            visible: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(None),
        }
    }

    /// Converts item properties to com.canonical.dbusmenu format.
    pub fn dbus_properties(&self) -> HashMap<String, DBusMenuPropValue> {
        let mut props = HashMap::new();

        if self.separator {
            props.insert(
                "type".to_string(),
                DBusMenuPropValue::String("separator".to_string()),
            );
            props.insert(
                "visible".to_string(),
                DBusMenuPropValue::Boolean(*self.visible.lock().unwrap()),
            );
            return props;
        }

        props.insert(
            "label".to_string(),
            DBusMenuPropValue::String(self.text.lock().unwrap().clone()),
        );
        props.insert(
            "enabled".to_string(),
            DBusMenuPropValue::Boolean(*self.enabled.lock().unwrap()),
        );
        props.insert(
            "visible".to_string(),
            DBusMenuPropValue::Boolean(*self.visible.lock().unwrap()),
        );

        if self.checkable {
            props.insert(
                "toggle-type".to_string(),
                DBusMenuPropValue::String("checkmark".to_string()),
            );
            let state = if *self.checked.lock().unwrap() { 1 } else { 0 };
            props.insert("toggle-state".to_string(), DBusMenuPropValue::Int(state));
        }

        if self.submenu.lock().unwrap().is_some() {
            props.insert(
                "children-display".to_string(),
                DBusMenuPropValue::String("submenu".to_string()),
            );
        }

        props
    }
}

impl PlatformMenuItem for DBusMenuItem {
    fn id(&self) -> u32 {
        self.id
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn set_text(&mut self, text: &str) {
        *self.text.lock().unwrap() = text.to_string();
    }

    fn is_separator(&self) -> bool {
        self.separator
    }

    fn is_checkable(&self) -> bool {
        self.checkable
    }

    fn is_checked(&self) -> bool {
        *self.checked.lock().unwrap()
    }

    fn set_checked(&mut self, checked: bool) {
        *self.checked.lock().unwrap() = checked;
    }

    fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn set_enabled(&mut self, enabled: bool) {
        *self.enabled.lock().unwrap() = enabled;
    }

    fn activated(&self) -> &Signal<()> {
        &self.activated
    }
}

pub struct DBusMenu {
    items: Mutex<Vec<Arc<DBusMenuItem>>>,
    revision: AtomicU32,
    is_popup_requested: Mutex<bool>,
}

impl DBusMenu {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            revision: AtomicU32::new(1),
            is_popup_requested: Mutex::new(false),
        }
    }

    pub fn revision(&self) -> u32 {
        self.revision.load(Ordering::SeqCst)
    }

    pub fn items(&self) -> Vec<Arc<DBusMenuItem>> {
        self.items.lock().unwrap().clone()
    }

    pub fn get_layout(&self, parent_id: u32, recursion_depth: i32) -> DBusMenuLayoutNode {
        let items = self.items.lock().unwrap();

        let mut children = Vec::new();
        if recursion_depth != 0 {
            for item in items.iter() {
                let props = item.dbus_properties();
                let sub_children = Vec::new();
                children.push(DBusMenuLayoutNode {
                    id: item.id,
                    properties: props,
                    children: sub_children,
                });
            }
        }

        let mut root_props = HashMap::new();
        root_props.insert(
            "children-display".to_string(),
            DBusMenuPropValue::String("submenu".to_string()),
        );

        DBusMenuLayoutNode {
            id: parent_id,
            properties: root_props,
            children,
        }
    }

    pub fn handle_event(&self, id: u32, event_id: &str) -> bool {
        // Release the items lock before emitting so slots may call back into the menu.
        let found = self.items.lock().unwrap().iter().find(|i| i.id == id).cloned();
        let Some(item) = found else {
            return false;
        };
        if event_id == "clicked" && *item.enabled.lock().unwrap() {
            if item.checkable {
                let mut chk = item.checked.lock().unwrap();
                *chk = !*chk;
                self.revision.fetch_add(1, Ordering::SeqCst);
            }
            item.activated.emit(&());
            return true;
        }
        false
    }
}

impl Default for DBusMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformMenu for DBusMenu {
    fn add_action(&mut self, id: u32, text: &str) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(DBusMenuItem::new_action(id, text));
        self.items.lock().unwrap().push(item.clone());
        self.revision.fetch_add(1, Ordering::SeqCst);
        item
    }

    fn add_checkable(&mut self, id: u32, text: &str, checked: bool) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(DBusMenuItem::new_checkable(id, text, checked));
        self.items.lock().unwrap().push(item.clone());
        self.revision.fetch_add(1, Ordering::SeqCst);
        item
    }

    fn add_separator(&mut self) {
        self.items
            .lock()
            .unwrap()
            .push(Arc::new(DBusMenuItem::new_separator()));
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn add_submenu(&mut self, text: &str, submenu: Box<dyn PlatformMenu>) {
        let item = Arc::new(DBusMenuItem {
            id: (self.items.lock().unwrap().len() + 1000) as u32,
            text: Mutex::new(text.to_string()),
            separator: false,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(true),
            visible: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(Some(submenu)),
        });
        self.items.lock().unwrap().push(item);
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn show_popup(&self, _screen_pos: Point) {
        // On Linux D-Bus architecture, client requests activation.
        // Send ItemActivationRequested signal.
        *self.is_popup_requested.lock().unwrap() = true;
    }

    fn dismiss(&self) {
        *self.is_popup_requested.lock().unwrap() = false;
    }
}
