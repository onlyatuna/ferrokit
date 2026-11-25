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
                DBusMenuPropValue::Boolean(*ferrokit_core::poison::lock_unpoisoned(&self.visible)),
            );
            return props;
        }

        props.insert(
            "label".to_string(),
            DBusMenuPropValue::String(ferrokit_core::poison::lock_unpoisoned(&self.text).clone()),
        );
        props.insert(
            "enabled".to_string(),
            DBusMenuPropValue::Boolean(*ferrokit_core::poison::lock_unpoisoned(&self.enabled)),
        );
        props.insert(
            "visible".to_string(),
            DBusMenuPropValue::Boolean(*ferrokit_core::poison::lock_unpoisoned(&self.visible)),
        );

        if self.checkable {
            props.insert(
                "toggle-type".to_string(),
                DBusMenuPropValue::String("checkmark".to_string()),
            );
            let state = if *ferrokit_core::poison::lock_unpoisoned(&self.checked) {
                1
            } else {
                0
            };
            props.insert("toggle-state".to_string(), DBusMenuPropValue::Int(state));
        }

        if ferrokit_core::poison::lock_unpoisoned(&self.submenu).is_some() {
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
        ferrokit_core::poison::lock_unpoisoned(&self.text).clone()
    }

    fn set_text(&mut self, text: &str) {
        *ferrokit_core::poison::lock_unpoisoned(&self.text) = text.to_string();
    }

    fn is_separator(&self) -> bool {
        self.separator
    }

    fn is_checkable(&self) -> bool {
        self.checkable
    }

    fn is_checked(&self) -> bool {
        *ferrokit_core::poison::lock_unpoisoned(&self.checked)
    }

    fn set_checked(&mut self, checked: bool) {
        *ferrokit_core::poison::lock_unpoisoned(&self.checked) = checked;
    }

    fn is_enabled(&self) -> bool {
        *ferrokit_core::poison::lock_unpoisoned(&self.enabled)
    }

    fn set_enabled(&mut self, enabled: bool) {
        *ferrokit_core::poison::lock_unpoisoned(&self.enabled) = enabled;
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
        ferrokit_core::poison::lock_unpoisoned(&self.items).clone()
    }

    pub fn get_layout(&self, parent_id: u32, recursion_depth: i32) -> DBusMenuLayoutNode {
        let items = ferrokit_core::poison::lock_unpoisoned(&self.items);

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
        let found = ferrokit_core::poison::lock_unpoisoned(&self.items)
            .iter()
            .find(|i| i.id == id)
            .cloned();
        let Some(item) = found else {
            return false;
        };
        if event_id == "clicked" && *ferrokit_core::poison::lock_unpoisoned(&item.enabled) {
            if item.checkable {
                let mut chk = ferrokit_core::poison::lock_unpoisoned(&item.checked);
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

// The `PlatformMenuItem` trait hands out `Arc`s; the items are only used from the GUI thread.
#[allow(clippy::arc_with_non_send_sync)]
impl PlatformMenu for DBusMenu {
    fn add_action(&mut self, id: u32, text: &str) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(DBusMenuItem::new_action(id, text));
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(item.clone());
        self.revision.fetch_add(1, Ordering::SeqCst);
        item
    }

    fn add_checkable(&mut self, id: u32, text: &str, checked: bool) -> Arc<dyn PlatformMenuItem> {
        let item = Arc::new(DBusMenuItem::new_checkable(id, text, checked));
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(item.clone());
        self.revision.fetch_add(1, Ordering::SeqCst);
        item
    }

    fn add_separator(&mut self) {
        ferrokit_core::poison::lock_unpoisoned(&self.items)
            .push(Arc::new(DBusMenuItem::new_separator()));
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn add_submenu(&mut self, text: &str, submenu: Box<dyn PlatformMenu>) {
        let item = Arc::new(DBusMenuItem {
            id: (ferrokit_core::poison::lock_unpoisoned(&self.items).len() + 1000) as u32,
            text: Mutex::new(text.to_string()),
            separator: false,
            checkable: false,
            checked: Mutex::new(false),
            enabled: Mutex::new(true),
            visible: Mutex::new(true),
            activated: Signal::new(),
            submenu: Mutex::new(Some(submenu)),
        });
        ferrokit_core::poison::lock_unpoisoned(&self.items).push(item);
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn show_popup(&self, _screen_pos: Point) {
        // On Linux D-Bus architecture, client requests activation.
        // Send ItemActivationRequested signal.
        *ferrokit_core::poison::lock_unpoisoned(&self.is_popup_requested) = true;
    }

    fn dismiss(&self) {
        *ferrokit_core::poison::lock_unpoisoned(&self.is_popup_requested) = false;
    }
}
