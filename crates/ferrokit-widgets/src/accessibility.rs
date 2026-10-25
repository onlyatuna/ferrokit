// SPDX-License-Identifier: LGPL-3.0-only
use crate::focus::FocusPolicy;
use crate::widget::WidgetRef;
use std::collections::HashMap;

/// Semantic role exposed to assistive technology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessibleRole {
    Application,
    Window,
    Dialog,
    Group,
    Button,
    CheckBox,
    RadioButton,
    Text,
    TextInput,
    Image,
    List,
    ListItem,
    Menu,
    MenuItem,
    Slider,
    ProgressIndicator,
    Table,
    Row,
    Cell,
    Custom,
}

/// Runtime properties of an accessible element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessibleState {
    Disabled,
    Focusable,
    Focused,
    Checked,
    Expanded,
    Selected,
    Busy,
    Invisible,
    ReadOnly,
    Required,
}

/// Operations an assistive-technology client can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessibleAction {
    Invoke,
    Focus,
    Increment,
    Decrement,
    Expand,
    Collapse,
    SetValue,
}

/// Stable identifier within an accessibility tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccessibleId(pub u64);

/// A semantic element. Parent/child structure is owned by [`AccessibleTree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleNode {
    pub id: AccessibleId,
    pub role: AccessibleRole,
    pub name: String,
    pub description: String,
    pub value: Option<String>,
    pub states: Vec<AccessibleState>,
    pub actions: Vec<AccessibleAction>,
}

impl AccessibleNode {
    pub fn new(id: AccessibleId, role: AccessibleRole, name: impl Into<String>) -> Self {
        Self {
            id,
            role,
            name: name.into(),
            description: String::new(),
            value: None,
            states: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub fn has_state(&self, state: AccessibleState) -> bool {
        self.states.contains(&state)
    }

    pub fn supports_action(&self, action: AccessibleAction) -> bool {
        self.actions.contains(&action)
    }
}

/// In-memory accessible hierarchy that platform bridges can query and mirror.
///
/// IDs are unique per tree. Every non-root node has exactly one parent; removal
/// deletes its entire subtree. Mutations reject missing IDs and cycles rather
/// than leaving a malformed tree for a platform accessibility provider.
#[derive(Debug, Default)]
pub struct AccessibleTree {
    root: Option<AccessibleId>,
    nodes: HashMap<AccessibleId, AccessibleNode>,
    parents: HashMap<AccessibleId, AccessibleId>,
    children: HashMap<AccessibleId, Vec<AccessibleId>>,
}

impl AccessibleTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a semantic snapshot from the current widget hierarchy.
    pub fn from_widget(root: &WidgetRef) -> Self {
        fn append(tree: &mut AccessibleTree, widget_ref: &WidgetRef, parent: Option<AccessibleId>) {
            let (id, node, children) = {
                let widget = widget_ref.borrow();
                let id = AccessibleId(widget.id().0);
                let mut node =
                    AccessibleNode::new(id, widget.accessible_role(), widget.accessible_name());
                node.description = widget.accessible_description();
                node.value = widget.accessible_value();
                node.actions = widget.accessible_actions();
                if !widget.is_enabled() {
                    node.states.push(AccessibleState::Disabled);
                }
                if !widget.is_visible() {
                    node.states.push(AccessibleState::Invisible);
                }
                if widget.has_focus() {
                    node.states.push(AccessibleState::Focused);
                }
                if widget.focus_policy() != FocusPolicy::NoFocus {
                    node.states.push(AccessibleState::Focusable);
                }
                (id, node, widget.children())
            };
            let inserted = if let Some(parent) = parent {
                tree.insert(parent, node).is_ok()
            } else {
                tree.set_root(node).is_ok()
            };
            if inserted {
                for child in children {
                    append(tree, &child, Some(id));
                }
            }
        }

        let mut tree = Self::new();
        append(&mut tree, root, None);
        tree
    }

    pub fn root(&self) -> Option<AccessibleId> {
        self.root
    }

    pub fn node(&self, id: AccessibleId) -> Option<&AccessibleNode> {
        self.nodes.get(&id)
    }

    pub fn parent(&self, id: AccessibleId) -> Option<AccessibleId> {
        self.parents.get(&id).copied()
    }

    pub fn children(&self, id: AccessibleId) -> &[AccessibleId] {
        self.children.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn set_root(&mut self, node: AccessibleNode) -> Result<(), Box<AccessibleNode>> {
        if self.root.is_some() || self.nodes.contains_key(&node.id) {
            return Err(Box::new(node));
        }
        self.root = Some(node.id);
        self.children.entry(node.id).or_default();
        self.nodes.insert(node.id, node);
        Ok(())
    }

    pub fn insert(
        &mut self,
        parent: AccessibleId,
        node: AccessibleNode,
    ) -> Result<(), Box<AccessibleNode>> {
        if !self.nodes.contains_key(&parent) || self.nodes.contains_key(&node.id) {
            return Err(Box::new(node));
        }
        self.children.entry(parent).or_default().push(node.id);
        self.children.entry(node.id).or_default();
        self.parents.insert(node.id, parent);
        self.nodes.insert(node.id, node);
        Ok(())
    }

    pub fn update(&mut self, node: AccessibleNode) -> Result<(), Box<AccessibleNode>> {
        if !self.nodes.contains_key(&node.id) {
            return Err(Box::new(node));
        }
        self.nodes.insert(node.id, node);
        Ok(())
    }

    pub fn reparent(&mut self, id: AccessibleId, new_parent: AccessibleId) -> bool {
        if id == self.root.unwrap_or(id)
            || !self.nodes.contains_key(&id)
            || !self.nodes.contains_key(&new_parent)
        {
            return false;
        }
        let mut ancestor = Some(new_parent);
        while let Some(current) = ancestor {
            if current == id {
                return false;
            }
            ancestor = self.parents.get(&current).copied();
        }

        let old_parent = match self.parents.get(&id).copied() {
            Some(parent) => parent,
            None => return false,
        };
        self.children
            .get_mut(&old_parent)
            .unwrap()
            .retain(|child| *child != id);
        self.children.entry(new_parent).or_default().push(id);
        self.parents.insert(id, new_parent);
        true
    }

    pub fn remove(&mut self, id: AccessibleId) -> bool {
        if self.root == Some(id) || !self.nodes.contains_key(&id) {
            return false;
        }
        if let Some(parent) = self.parents.remove(&id) {
            self.children
                .get_mut(&parent)
                .unwrap()
                .retain(|child| *child != id);
        }
        let mut pending = vec![id];
        while let Some(current) = pending.pop() {
            if let Some(children) = self.children.remove(&current) {
                pending.extend(children);
            }
            self.parents.remove(&current);
            self.nodes.remove(&current);
        }
        true
    }
}
