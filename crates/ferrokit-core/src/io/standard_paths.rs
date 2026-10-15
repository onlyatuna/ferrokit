// SPDX-License-Identifier: LGPL-3.0-only
//! StandardPaths cross-platform location resolver (`QStandardPaths` equivalent).
//!
//! Provides resolution of standard operating system directories, such as Desktop,
//! Documents, AppData, Cache, Config, Temp, and executable path resolution.

use std::env;
use std::path::PathBuf;

/// Enumeration of standard location types (`QStandardPaths::StandardLocation` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardLocation {
    DesktopLocation,
    DocumentsLocation,
    FontsLocation,
    ApplicationsLocation,
    MusicLocation,
    MoviesLocation,
    PicturesLocation,
    TempLocation,
    HomeLocation,
    AppLocalDataLocation,
    CacheLocation,
    GenericDataLocation,
    RuntimeLocation,
    ConfigLocation,
    DownloadLocation,
    GenericCacheLocation,
    GenericConfigLocation,
    AppDataLocation,
    AppConfigLocation,
}

/// Standard paths resolver matching Qt `QStandardPaths`.
pub struct StandardPaths;

impl StandardPaths {
    /// Resolves the single primary writable directory for the specified location.
    pub fn writable_location(location: StandardLocation) -> PathBuf {
        let home = Self::home_dir();

        match location {
            StandardLocation::HomeLocation => home,

            StandardLocation::TempLocation => env::temp_dir(),

            StandardLocation::DesktopLocation => {
                #[cfg(windows)]
                {
                    home.join("Desktop")
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Desktop")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::xdg_user_dir("DESKTOP").unwrap_or_else(|| home.join("Desktop"))
                }
            }

            StandardLocation::DocumentsLocation => {
                #[cfg(windows)]
                {
                    home.join("Documents")
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Documents")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::xdg_user_dir("DOCUMENTS").unwrap_or_else(|| home.join("Documents"))
                }
            }

            StandardLocation::DownloadLocation => {
                #[cfg(windows)]
                {
                    home.join("Downloads")
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Downloads")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::xdg_user_dir("DOWNLOAD").unwrap_or_else(|| home.join("Downloads"))
                }
            }

            StandardLocation::MusicLocation => {
                #[cfg(windows)]
                {
                    home.join("Music")
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Music")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::xdg_user_dir("MUSIC").unwrap_or_else(|| home.join("Music"))
                }
            }

            StandardLocation::MoviesLocation => {
                #[cfg(windows)]
                {
                    home.join("Videos")
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Movies")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::xdg_user_dir("VIDEOS").unwrap_or_else(|| home.join("Videos"))
                }
            }

            StandardLocation::PicturesLocation => {
                #[cfg(windows)]
                {
                    home.join("Pictures")
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Pictures")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::xdg_user_dir("PICTURES").unwrap_or_else(|| home.join("Pictures"))
                }
            }

            StandardLocation::FontsLocation => {
                #[cfg(windows)]
                {
                    env::var("WINDIR")
                        .map(|w| PathBuf::from(w).join("Fonts"))
                        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows\Fonts"))
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Library/Fonts")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    home.join(".local/share/fonts")
                }
            }

            StandardLocation::ApplicationsLocation => {
                #[cfg(windows)]
                {
                    Self::var_path("APPDATA")
                        .map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs"))
                        .unwrap_or_else(|| home.join("Programs"))
                }
                #[cfg(target_os = "macos")]
                {
                    PathBuf::from("/Applications")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::var_path("XDG_DATA_HOME")
                        .unwrap_or_else(|| home.join(".local/share"))
                        .join("applications")
                }
            }

            StandardLocation::AppDataLocation
            | StandardLocation::AppLocalDataLocation
            | StandardLocation::GenericDataLocation => {
                #[cfg(windows)]
                {
                    Self::var_path("LOCALAPPDATA")
                        .or_else(|| Self::var_path("APPDATA"))
                        .unwrap_or_else(|| home.join("AppData/Local"))
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Library/Application Support")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::var_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"))
                }
            }

            StandardLocation::CacheLocation | StandardLocation::GenericCacheLocation => {
                #[cfg(windows)]
                {
                    Self::var_path("LOCALAPPDATA")
                        .map(|p| p.join("cache"))
                        .unwrap_or_else(env::temp_dir)
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Library/Caches")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::var_path("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"))
                }
            }

            StandardLocation::ConfigLocation
            | StandardLocation::AppConfigLocation
            | StandardLocation::GenericConfigLocation => {
                #[cfg(windows)]
                {
                    Self::var_path("APPDATA").unwrap_or_else(|| home.join("AppData/Roaming"))
                }
                #[cfg(target_os = "macos")]
                {
                    home.join("Library/Preferences")
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::var_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"))
                }
            }

            StandardLocation::RuntimeLocation => {
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    Self::var_path("XDG_RUNTIME_DIR").unwrap_or_else(env::temp_dir)
                }
                #[cfg(any(windows, target_os = "macos"))]
                {
                    env::temp_dir()
                }
            }
        }
    }

    /// Resolves all candidate directories for the specified location (including system-wide dirs).
    pub fn standard_locations(location: StandardLocation) -> Vec<PathBuf> {
        #[allow(unused_mut)]
        let mut dirs = vec![Self::writable_location(location)];

        match location {
            StandardLocation::FontsLocation => {
                #[cfg(target_os = "macos")]
                {
                    dirs.push(PathBuf::from("/Library/Fonts"));
                    dirs.push(PathBuf::from("/System/Library/Fonts"));
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    dirs.push(PathBuf::from("/usr/share/fonts"));
                    dirs.push(PathBuf::from("/usr/local/share/fonts"));
                }
            }
            StandardLocation::ApplicationsLocation => {
                #[cfg(target_os = "macos")]
                {
                    dirs.push(Self::home_dir().join("Applications"));
                }
                #[cfg(all(not(windows), not(target_os = "macos")))]
                {
                    dirs.push(PathBuf::from("/usr/share/applications"));
                    dirs.push(PathBuf::from("/usr/local/share/applications"));
                }
            }
            _ => {}
        }

        dirs
    }

    /// Finds the first file matching `file_name` in any directory for `location`.
    pub fn locate(location: StandardLocation, file_name: &str) -> Option<PathBuf> {
        for dir in Self::standard_locations(location) {
            let candidate = dir.join(file_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    /// Finds all files matching `file_name` across all directories for `location`.
    pub fn locate_all(location: StandardLocation, file_name: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for dir in Self::standard_locations(location) {
            let candidate = dir.join(file_name);
            if candidate.exists() {
                found.push(candidate);
            }
        }
        found
    }

    /// Finds an executable binary in the system `PATH` (`findExecutable` equivalent).
    pub fn find_executable(name: &str) -> Option<PathBuf> {
        let path_var = env::var_os("PATH")?;

        #[cfg(windows)]
        let extensions: Vec<String> = {
            let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string());
            pathext.split(';').map(|s| s.to_lowercase()).collect()
        };

        for dir in env::split_paths(&path_var) {
            let direct = dir.join(name);
            if direct.is_file() {
                return Some(direct);
            }

            #[cfg(windows)]
            {
                // If name doesn't already have extension, try PATHEXT
                if !name.contains('.') {
                    for ext in &extensions {
                        let with_ext = dir.join(format!("{}{}", name, ext));
                        if with_ext.is_file() {
                            return Some(with_ext);
                        }
                    }
                }
            }
        }
        None
    }

    fn home_dir() -> PathBuf {
        Self::var_path("USERPROFILE")
            .or_else(|| Self::var_path("HOME"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn var_path(key: &str) -> Option<PathBuf> {
        env::var(key).ok().map(PathBuf::from)
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    fn xdg_user_dir(_key: &str) -> Option<PathBuf> {
        None
    }
}
