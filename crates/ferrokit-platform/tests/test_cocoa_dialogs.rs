// SPDX-License-Identifier: LGPL-3.0-only
//! The AppKit dialog logic (`NSOpenPanel`/`NSSavePanel`/`NSAlert`) against a recording
//! `DialogRuntime`: the message sequence, argument values, result mapping, main-thread guard and
//! the autorelease-pool / `release` balance are checked on every OS, without AppKit.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use ferrokit_platform::dialogs_cocoa::{allowed_extensions, CocoaDialogs, DialogRuntime};
use ferrokit_platform::objc_runtime::{Id, NSInteger};
use ferrokit_platform::{
    DialogIcon, FileFilter, MessageBoxButtons, MessageBoxResult, PlatformDialogs,
};

#[derive(Default)]
struct State {
    next: usize,
    names: HashMap<usize, String>,
    strings: HashMap<usize, String>,
    arrays: HashMap<usize, Vec<usize>>,
    url_paths: HashMap<usize, String>,
    log: Vec<String>,
    responses: VecDeque<NSInteger>,
    result_paths: Vec<String>,
    pools_open: usize,
    pools_popped: usize,
    allocated: Vec<usize>,
    released: Vec<usize>,
    not_main: bool,
    policy: NSInteger,
}

#[derive(Default)]
struct Fake(Mutex<State>);

fn id(n: usize) -> Id {
    Id(n as *mut std::ffi::c_void)
}

impl State {
    fn make(&mut self, name: impl Into<String>) -> usize {
        self.next += 1;
        self.names.insert(self.next, name.into());
        self.next
    }
    fn name(&self, obj: Id) -> String {
        self.names
            .get(&(obj.0 as usize))
            .cloned()
            .unwrap_or_else(|| "nil".to_string())
    }
    fn path_url(&mut self, path: &str) -> usize {
        let n = self.make(format!("NSURL({path})"));
        self.url_paths.insert(n, path.to_string());
        n
    }
}

impl Fake {
    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
    fn respond(&self, response: NSInteger, paths: &[&str]) {
        self.with(|s| {
            s.responses.push_back(response);
            s.result_paths = paths.iter().map(|p| p.to_string()).collect();
        });
    }
    fn log(&self) -> Vec<String> {
        self.with(|s| s.log.clone())
    }
}

impl DialogRuntime for Fake {
    fn require_main_thread(&self, operation: &str) {
        if self.with(|s| s.not_main) {
            panic!("{operation} must be called on the main thread");
        }
    }
    fn pool_push(&self) -> Id {
        self.with(|s| {
            s.pools_open += 1;
            s.log.push("pool push".into());
            id(s.make("pool"))
        })
    }
    fn pool_pop(&self, _pool: Id) {
        self.with(|s| {
            s.pools_open -= 1;
            s.pools_popped += 1;
            s.log.push("pool pop".into());
        });
    }
    fn shared_application(&self) -> Id {
        self.with(|s| {
            s.log.push("+[NSApplication sharedApplication]".into());
            id(s.make("NSApp"))
        })
    }
    fn class_msg(&self, class: &str, sel: &str) -> Id {
        self.with(|s| {
            s.log.push(format!("+[{class} {sel}]"));
            id(s.make(class))
        })
    }
    fn class_msg_id(&self, class: &str, sel: &str, arg: Id) -> Id {
        self.with(|s| {
            let arg_text = s
                .strings
                .get(&(arg.0 as usize))
                .cloned()
                .unwrap_or_default();
            s.log.push(format!("+[{class} {sel} {arg_text:?}]"));
            id(s.path_url(&arg_text))
        })
    }
    fn alloc_init(&self, class: &str) -> Id {
        self.with(|s| {
            s.log.push(format!("[[{class} alloc] init]"));
            let n = s.make(class);
            s.allocated.push(n);
            id(n)
        })
    }
    fn msg(&self, recv: Id, sel: &str) -> Id {
        self.with(|s| {
            let name = s.name(recv);
            s.log.push(format!("[{name} {sel}]"));
            match sel {
                "URLs" => {
                    let paths = s.result_paths.clone();
                    let urls = paths.iter().map(|p| s.path_url(p)).collect();
                    let array = s.make("NSArray");
                    s.arrays.insert(array, urls);
                    id(array)
                }
                "URL" => match s.result_paths.first().cloned() {
                    Some(p) => id(s.path_url(&p)),
                    None => Id::NIL,
                },
                "path" => {
                    let path = s.url_paths[&(recv.0 as usize)].clone();
                    let n = s.make("NSString");
                    s.strings.insert(n, path);
                    id(n)
                }
                _ => id(s.make(format!("{name}.{sel}"))),
            }
        })
    }
    fn msg_id(&self, recv: Id, sel: &str, arg: Id) -> Id {
        self.with(|s| {
            let name = s.name(recv);
            let arg_text = if let Some(text) = s.strings.get(&(arg.0 as usize)) {
                format!("{text:?}")
            } else if let Some(items) = s.arrays.get(&(arg.0 as usize)) {
                let texts: Vec<_> = items.iter().map(|i| s.strings[i].clone()).collect();
                format!("{texts:?}")
            } else {
                s.name(arg)
            };
            s.log.push(format!("[{name} {sel} {arg_text}]"));
            if sel == "addButtonWithTitle:" {
                let title = s.strings[&(arg.0 as usize)].clone();
                return id(s.make(format!("button {title}")));
            }
            Id::NIL
        })
    }
    fn msg_bool(&self, recv: Id, sel: &str, value: bool) {
        self.with(|s| {
            let name = s.name(recv);
            s.log.push(format!("[{name} {sel} {value}]"));
        });
    }
    fn msg_int(&self, recv: Id, sel: &str, value: NSInteger) {
        self.with(|s| {
            let name = s.name(recv);
            s.log.push(format!("[{name} {sel} {value}]"));
        });
    }
    fn msg_int_return(&self, recv: Id, sel: &str) -> NSInteger {
        let response = self.with(|s| {
            let name = s.name(recv);
            s.log.push(format!("[{name} {sel}]"));
            if sel == "activationPolicy" {
                return Some(s.policy);
            }
            s.responses.pop_front()
        });
        response.expect("a scripted modal response")
    }
    fn ns_string(&self, text: &str) -> Id {
        self.with(|s| {
            let n = s.make("NSString");
            s.strings.insert(n, text.to_string());
            id(n)
        })
    }
    fn string_value(&self, string: Id) -> Option<String> {
        self.with(|s| s.strings.get(&(string.0 as usize)).cloned())
    }
    fn array(&self, items: &[Id]) -> Id {
        self.with(|s| {
            let n = s.make("NSArray");
            s.arrays
                .insert(n, items.iter().map(|i| i.0 as usize).collect());
            id(n)
        })
    }
    fn array_len(&self, array: Id) -> usize {
        self.with(|s| s.arrays[&(array.0 as usize)].len())
    }
    fn array_at(&self, array: Id, index: usize) -> Id {
        self.with(|s| id(s.arrays[&(array.0 as usize)][index]))
    }
    fn release(&self, object: Id) {
        self.with(|s| {
            s.log.push("release".into());
            s.released.push(object.0 as usize);
        });
    }
}

fn dialogs() -> CocoaDialogs<Fake> {
    CocoaDialogs::new(Fake::default())
}

fn rt(d: &CocoaDialogs<Fake>) -> &Fake {
    d.runtime()
}

fn png_filter() -> FileFilter {
    FileFilter {
        patterns: vec!["*.png".into(), "*.jpg".into()],
        description: "Images".into(),
    }
}

fn assert_balanced(fake: &Fake) {
    fake.with(|s| {
        assert_eq!(s.pools_open, 0, "autorelease pool left open");
        assert_eq!(s.pools_popped, 1);
        let mut a = s.allocated.clone();
        let mut r = s.released.clone();
        a.sort();
        r.sort();
        assert_eq!(a, r, "every alloc'ed object is released exactly once");
    });
}

#[test]
fn open_file_message_sequence_and_result() {
    let d = dialogs();
    let dir = std::env::temp_dir();
    let dir_s = dir.to_string_lossy().into_owned();
    rt(&d).respond(1, &["/Users/me/a.png"]);
    let got = d.open_file("Pick", &dir_s, Some(&png_filter()));
    assert_eq!(got, Some(PathBuf::from("/Users/me/a.png")));
    assert_eq!(
        rt(&d).log(),
        vec![
            "pool push".to_string(),
            "+[NSApplication sharedApplication]".into(),
            "[NSApp activationPolicy]".into(),
            "[NSApp activateIgnoringOtherApps: true]".into(),
            "+[NSOpenPanel openPanel]".into(),
            "[NSOpenPanel setMessage: \"Pick\"]".into(),
            "[NSOpenPanel setCanChooseFiles: true]".into(),
            "[NSOpenPanel setCanChooseDirectories: false]".into(),
            "[NSOpenPanel setAllowsMultipleSelection: false]".into(),
            "[NSOpenPanel setAllowedFileTypes: [\"png\", \"jpg\"]]".into(),
            format!("+[NSURL fileURLWithPath: {dir_s:?}]"),
            format!("[NSOpenPanel setDirectoryURL: NSURL({dir_s})]"),
            "[NSOpenPanel runModal]".into(),
            "[NSOpenPanel URL]".into(),
            "[NSURL(/Users/me/a.png) path]".into(),
            "pool pop".into(),
        ]
    );
    assert_balanced(rt(&d));
}

#[test]
fn open_files_returns_all_urls_in_order_and_cancel_is_none() {
    let d = dialogs();
    rt(&d).respond(1, &["/a/1.txt", "/a/2.txt", "/b/3.txt"]);
    let got = d.open_files("", "", None);
    assert_eq!(
        got,
        Some(vec![
            PathBuf::from("/a/1.txt"),
            PathBuf::from("/a/2.txt"),
            PathBuf::from("/b/3.txt")
        ])
    );
    let log = rt(&d).log();
    assert!(log.contains(&"[NSOpenPanel setAllowsMultipleSelection: true]".to_string()));
    assert!(log.contains(&"[NSOpenPanel URLs]".to_string()));
    assert!(!log
        .iter()
        .any(|l| l.contains("setMessage:") || l.contains("setDirectoryURL:")));

    let cancelled = dialogs();
    rt(&cancelled).respond(0, &["/ignored"]);
    assert_eq!(cancelled.open_files("t", "", None), None);
    assert!(!rt(&cancelled).log().iter().any(|l| l.contains("URLs")));
    assert_balanced(rt(&cancelled));
}

#[test]
fn save_file_sets_name_directory_and_ignores_cancel() {
    let d = dialogs();
    let dir = std::env::temp_dir();
    let initial = dir.join("report.txt");
    let (dir_s, initial_s) = (
        initial.parent().unwrap().to_string_lossy().into_owned(),
        initial.to_string_lossy().into_owned(),
    );
    rt(&d).respond(1, &["/out/report.txt"]);
    let got = d.save_file("Save", &initial_s, None);
    assert_eq!(got, Some(PathBuf::from("/out/report.txt")));
    let log = rt(&d).log();
    assert!(log.contains(&"+[NSSavePanel savePanel]".to_string()));
    assert!(log.contains(&"[NSSavePanel setCanCreateDirectories: true]".to_string()));
    assert!(log.contains(&format!("[NSSavePanel setDirectoryURL: NSURL({dir_s})]")));
    assert!(log.contains(&"[NSSavePanel setNameFieldStringValue: \"report.txt\"]".to_string()));
    assert!(!log.iter().any(|l| l.contains("setAllowedFileTypes:")));
    assert!(!log.iter().any(|l| l.contains("NSOpenPanel")));
    assert_balanced(rt(&d));

    let cancelled = dialogs();
    rt(&cancelled).respond(0, &[]);
    assert_eq!(cancelled.save_file("Save", "", None), None);
}

#[test]
fn select_folder_chooses_directories_only() {
    let d = dialogs();
    rt(&d).respond(1, &["/Users/me/dir"]);
    assert_eq!(
        d.select_folder("Folder", ""),
        Some(PathBuf::from("/Users/me/dir"))
    );
    let log = rt(&d).log();
    assert!(log.contains(&"[NSOpenPanel setCanChooseFiles: false]".to_string()));
    assert!(log.contains(&"[NSOpenPanel setCanChooseDirectories: true]".to_string()));
    assert!(log.contains(&"[NSOpenPanel setCanCreateDirectories: true]".to_string()));
    assert!(!log.iter().any(|l| l.contains("setAllowedFileTypes:")));
    assert_balanced(rt(&d));
}

#[test]
fn message_box_buttons_style_and_result_mapping() {
    // (buttons, modal response, expected result)
    let cases = [
        (MessageBoxButtons::Ok, 1000, MessageBoxResult::Ok),
        (MessageBoxButtons::OkCancel, 1000, MessageBoxResult::Ok),
        (MessageBoxButtons::OkCancel, 1001, MessageBoxResult::Cancel),
        (MessageBoxButtons::YesNo, 1000, MessageBoxResult::Yes),
        (MessageBoxButtons::YesNo, 1001, MessageBoxResult::No),
        // Stopped/aborted alerts and unknown responses fall back to the last button.
        (MessageBoxButtons::Ok, -1001, MessageBoxResult::Ok),
        (MessageBoxButtons::OkCancel, -1000, MessageBoxResult::Cancel),
        (MessageBoxButtons::YesNo, 1002, MessageBoxResult::No),
    ];
    for (buttons, response, expected) in cases {
        let d = dialogs();
        rt(&d).respond(response, &[]);
        let got = d.message_box("Title", "Body", DialogIcon::Warning, buttons);
        assert_eq!(got, expected, "{buttons:?} response {response}");
        assert_balanced(rt(&d));
    }
}

#[test]
fn message_box_sequence() {
    let d = dialogs();
    rt(&d).respond(1001, &[]);
    let got = d.message_box(
        "Quit?",
        "Unsaved",
        DialogIcon::Error,
        MessageBoxButtons::YesNo,
    );
    assert_eq!(got, MessageBoxResult::No);
    assert_eq!(
        rt(&d).log(),
        vec![
            "pool push".to_string(),
            "+[NSApplication sharedApplication]".into(),
            "[NSApp activationPolicy]".into(),
            "[NSApp activateIgnoringOtherApps: true]".into(),
            "[[NSAlert alloc] init]".into(),
            "[NSAlert setAlertStyle: 2]".into(),
            "[NSAlert setMessageText: \"Quit?\"]".into(),
            "[NSAlert setInformativeText: \"Unsaved\"]".into(),
            "[NSAlert addButtonWithTitle: \"Yes\"]".into(),
            "[NSAlert addButtonWithTitle: \"No\"]".into(),
            "[button No setKeyEquivalent: \"\\u{1b}\"]".into(),
            "[NSAlert runModal]".into(),
            "release".into(),
            "pool pop".into(),
        ]
    );
}

#[test]
fn icon_maps_to_alert_style() {
    for (icon, style) in [
        (DialogIcon::Warning, 0),
        (DialogIcon::Info, 1),
        (DialogIcon::Question, 1),
        (DialogIcon::Error, 2),
    ] {
        let d = dialogs();
        rt(&d).respond(1000, &[]);
        d.message_box("t", "m", icon, MessageBoxButtons::Ok);
        assert!(rt(&d)
            .log()
            .contains(&format!("[NSAlert setAlertStyle: {style}]")));
    }
}

#[test]
fn off_main_thread_panics_before_any_message() {
    let d = dialogs();
    rt(&d).with(|s| s.not_main = true);
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.open_file("t", "", None)));
    assert!(result.is_err());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        d.message_box("t", "m", DialogIcon::Info, MessageBoxButtons::Ok)
    }));
    assert!(result.is_err());
    assert!(rt(&d).log().is_empty(), "no Objective-C message was sent");
}

#[test]
fn pool_is_popped_when_a_panel_step_panics() {
    // No scripted response: the fake panics inside `runModal`.
    let d = dialogs();
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.select_folder("t", "")));
    assert!(result.is_err());
    rt(&d).with(|s| {
        assert_eq!(s.pools_open, 0);
        assert_eq!(s.pools_popped, 1);
    });
}

#[test]
fn filter_patterns_become_extensions() {
    let f = |patterns: &[&str]| {
        allowed_extensions(Some(&FileFilter {
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            description: String::new(),
        }))
    };
    assert_eq!(
        f(&["*.png", "*.PNG", "*.png"]),
        Some(vec!["png".into(), "PNG".into()])
    );
    assert_eq!(f(&[".txt", "md"]), Some(vec!["txt".into(), "md".into()]));
    assert_eq!(f(&["*.tar.gz"]), Some(vec!["gz".into()]));
    assert_eq!(f(&["*.png", "*"]), None);
    assert_eq!(f(&["*.*"]), None);
    assert_eq!(f(&["img*"]), None);
    assert_eq!(f(&[]), None);
    assert_eq!(allowed_extensions(None), None);
}

fn activation_log(policy: NSInteger, run: impl FnOnce(&CocoaDialogs<Fake>)) -> Vec<String> {
    let d = dialogs();
    rt(&d).with(|s| s.policy = policy);
    rt(&d).respond(1000, &[]);
    run(&d);
    rt(&d)
        .log()
        .into_iter()
        .filter(|l| l.starts_with("[NSApp"))
        .collect()
}

#[test]
fn prohibited_process_becomes_regular_and_activates_before_the_dialog() {
    for run in [
        (|d: &CocoaDialogs<Fake>| {
            d.message_box("t", "m", DialogIcon::Info, MessageBoxButtons::Ok);
        }) as fn(&CocoaDialogs<Fake>),
        |d| {
            d.select_folder("t", "");
        },
    ] {
        let d = dialogs();
        rt(&d).with(|s| s.policy = 2);
        rt(&d).respond(1000, &[]);
        run(&d);
        let log = rt(&d).log();
        let pos = |needle: &str| {
            log.iter()
                .position(|l| l == needle)
                .unwrap_or_else(|| panic!("{needle} missing in {log:?}"))
        };
        assert!(pos("[NSApp activationPolicy]") < pos("[NSApp setActivationPolicy: 0]"));
        assert!(
            pos("[NSApp setActivationPolicy: 0]") < pos("[NSApp activateIgnoringOtherApps: true]")
        );
        assert!(
            pos("[NSApp activateIgnoringOtherApps: true]")
                < log.iter().position(|l| l.ends_with("runModal]")).unwrap()
        );
    }
}

#[test]
fn bundled_app_policy_is_left_alone() {
    for policy in [0, 1] {
        let log = activation_log(policy, |d| {
            d.message_box("t", "m", DialogIcon::Info, MessageBoxButtons::Ok);
        });
        assert_eq!(
            log,
            vec![
                "[NSApp activationPolicy]".to_string(),
                "[NSApp activateIgnoringOtherApps: true]".into()
            ],
            "policy {policy}"
        );
    }
}

#[test]
fn empty_title_uses_the_message_as_headline() {
    let d = dialogs();
    rt(&d).respond(1000, &[]);
    d.message_box("", "Body only", DialogIcon::Info, MessageBoxButtons::Ok);
    let log = rt(&d).log();
    assert!(log.contains(&"[NSAlert setMessageText: \"Body only\"]".to_string()));
    assert!(!log.iter().any(|l| l.contains("setInformativeText:")));
    assert!(!log.iter().any(|l| l.contains("setMessageText: \"\"")));

    let d = dialogs();
    rt(&d).respond(1000, &[]);
    d.message_box("", "", DialogIcon::Info, MessageBoxButtons::Ok);
    assert!(!rt(&d).log().iter().any(|l| l.contains("Text:")));
}

#[test]
fn alert_is_released_before_the_pool_pops_when_run_modal_panics() {
    // No scripted response: the fake panics inside `runModal`.
    let d = dialogs();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        d.message_box("t", "m", DialogIcon::Info, MessageBoxButtons::Ok)
    }));
    assert!(result.is_err());
    let log = rt(&d).log();
    let n = log.len();
    assert_eq!(&log[n - 2..], ["release", "pool pop"]);
    assert_balanced(rt(&d));
}
