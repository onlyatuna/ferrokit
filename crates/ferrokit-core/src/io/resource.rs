// SPDX-License-Identifier: LGPL-3.0-only
//! QResource virtual embedded resource system (`QResource` and `qrc` equivalent).
//!
//! Provides in-memory virtual file system access to bundled assets via
//! virtual resource paths like `:/images/icon.png` or `qrc:/style/main.css`.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, RwLock};

use super::device::{IODevice, OpenMode};
use super::path::clean_path;
use crate::types::ByteArray;

static RESOURCE_REGISTRY: RwLock<Option<HashMap<String, Arc<[u8]>>>> = RwLock::new(None);

fn with_registry<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, Arc<[u8]>>) -> R,
{
    let mut lock = RESOURCE_REGISTRY.write().unwrap();
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    f(lock.as_mut().unwrap())
}

/// Normalizes resource paths: removes `qrc:` or `:` prefix and cleans path.
pub fn normalize_resource_path(path: &str) -> String {
    let stripped = if let Some(rest) = path.strip_prefix("qrc:") {
        rest
    } else if let Some(rest) = path.strip_prefix(':') {
        rest
    } else {
        path
    };
    let cleaned = clean_path(stripped);
    if cleaned.starts_with('/') {
        cleaned
    } else {
        format!("/{}", cleaned)
    }
}

/// Virtual Resource system manager (`QResource` equivalent).
pub struct Resource;

impl Resource {
    /// Registers in-memory binary data under the specified virtual path (e.g. `:/icons/app.png`).
    pub fn register_data(path: &str, data: impl Into<Arc<[u8]>>) {
        let norm_path = normalize_resource_path(path);
        let arc_data = data.into();
        with_registry(|reg| {
            reg.insert(norm_path, arc_data);
        });
    }

    /// Registers a slice of bytes into the resource system.
    pub fn register_bytes(path: &str, data: &[u8]) {
        let arc: Arc<[u8]> = Arc::from(data);
        Self::register_data(path, arc);
    }

    /// Registers a UTF-8 text string into the resource system.
    pub fn register_text(path: &str, text: &str) {
        Self::register_bytes(path, text.as_bytes());
    }

    /// Unregisters the resource at `path`. Returns `true` if it existed.
    pub fn unregister(path: &str) -> bool {
        let norm_path = normalize_resource_path(path);
        with_registry(|reg| reg.remove(&norm_path).is_some())
    }

    /// Returns `true` if a resource exists at `path`.
    pub fn exists(path: &str) -> bool {
        let norm_path = normalize_resource_path(path);
        let lock = RESOURCE_REGISTRY.read().unwrap();
        lock.as_ref()
            .map(|reg| reg.contains_key(&norm_path))
            .unwrap_or(false)
    }

    /// Retrieves an Arc clone of the resource data payload.
    pub fn data(path: &str) -> Option<Arc<[u8]>> {
        let norm_path = normalize_resource_path(path);
        let lock = RESOURCE_REGISTRY.read().unwrap();
        lock.as_ref().and_then(|reg| reg.get(&norm_path).cloned())
    }

    /// Retrieves the resource content as a `ByteArray`.
    pub fn read_bytes(path: &str) -> Option<ByteArray> {
        Self::data(path).map(|d| ByteArray::from(&*d))
    }

    /// Retrieves the resource content as a UTF-8 `String`.
    pub fn read_text(path: &str) -> Option<String> {
        Self::data(path).and_then(|d| String::from_utf8(d.to_vec()).ok())
    }

    /// Lists direct children virtual paths under the specified directory prefix.
    pub fn children(prefix: &str) -> Vec<String> {
        let norm_prefix = normalize_resource_path(prefix);
        let prefix_slash = if norm_prefix.ends_with('/') {
            norm_prefix.clone()
        } else {
            format!("{}/", norm_prefix)
        };

        let lock = RESOURCE_REGISTRY.read().unwrap();
        let mut results = Vec::new();
        if let Some(ref reg) = *lock {
            for key in reg.keys() {
                if let Some(rel) = key.strip_prefix(&prefix_slash) {
                    let direct_child = rel.split('/').next().unwrap_or("");
                    if !direct_child.is_empty() && !results.contains(&direct_child.to_string()) {
                        results.push(direct_child.to_string());
                    }
                }
            }
        }
        results.sort();
        results
    }

    /// Clears all registered resources.
    pub fn clear() {
        with_registry(|reg| reg.clear());
    }
}

/// Read-only IODevice for reading virtual resources (`QResource` file handle equivalent).
#[derive(Debug, Clone)]
pub struct ResourceFile {
    path: String,
    data: Option<Arc<[u8]>>,
    pos: u64,
    open_mode: OpenMode,
}

impl ResourceFile {
    /// Creates a new `ResourceFile` referencing the resource path (e.g. `:/app.qml`).
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            data: None,
            pos: 0,
            open_mode: OpenMode::NOT_OPEN,
        }
    }

    /// Opens the resource file in ReadOnly mode.
    pub fn open(&mut self) -> io::Result<()> {
        if let Some(payload) = Resource::data(&self.path) {
            self.data = Some(payload);
            self.pos = 0;
            self.open_mode = OpenMode::READ_ONLY;
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Resource not found: {}", self.path),
            ))
        }
    }
}

impl IODevice for ResourceFile {
    fn open_mode(&self) -> OpenMode {
        self.open_mode
    }

    fn pos(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> u64 {
        self.data.as_ref().map(|d| d.len() as u64).unwrap_or(0)
    }

    fn seek(&mut self, pos: u64) -> io::Result<()> {
        if !self.is_open() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "ResourceFile is not open",
            ));
        }
        self.pos = pos;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.is_open() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "ResourceFile is not open",
            ));
        }
        if let Some(ref data) = self.data {
            let cur = self.pos as usize;
            if cur >= data.len() {
                return Ok(0);
            }
            let avail = data.len() - cur;
            let to_read = std::cmp::min(buf.len(), avail);
            buf[..to_read].copy_from_slice(&data[cur..cur + to_read]);
            self.pos += to_read as u64;
            Ok(to_read)
        } else {
            Ok(0)
        }
    }

    fn write(&mut self, _data: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Resource files are read-only",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn close(&mut self) {
        self.open_mode = OpenMode::NOT_OPEN;
        self.data = None;
        self.pos = 0;
    }
}
