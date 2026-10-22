// SPDX-License-Identifier: LGPL-3.0-only
//! Integration tests for ferrokit-core I/O subsystem.
//!
//! Validates IODevice, Buffer, File, SaveFile, Path, Directory, FileInfo, DirIterator,
//! Resource (qrc), StandardPaths, Url & UrlQuery, Process, TemporaryFile/Dir, LockFile, and StorageInfo.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use ferrokit_core::io::*;
use ferrokit_core::types::*;

// =============================================================================
// 1. Buffer IODevice
// =============================================================================

#[test]
fn test_buffer_iodevice_operations() {
    let mut buf = Buffer::new();
    assert!(!buf.is_open());
    assert_eq!(buf.size(), 0);

    // Open for ReadWrite
    buf.open(OpenMode::READ_WRITE).expect("open buffer");
    assert!(buf.is_open());
    assert!(buf.is_readable());
    assert!(buf.is_writable());

    // Write text
    let written = buf.write(b"Hello Qt and Rust!").expect("write bytes");
    assert_eq!(written, 18);
    assert_eq!(buf.size(), 18);
    assert_eq!(buf.pos(), 18);
    assert!(buf.at_end());

    // Seek back to start
    buf.seek(0).expect("seek start");
    assert_eq!(buf.pos(), 0);
    assert!(!buf.at_end());

    // Read partial
    let mut chunk = [0u8; 5];
    let n = buf.read(&mut chunk).expect("read chunk");
    assert_eq!(n, 5);
    assert_eq!(&chunk, b"Hello");
    assert_eq!(buf.pos(), 5);

    // Read remaining
    let rem = buf.read_all().expect("read all");
    assert_eq!(&rem, b" Qt and Rust!");
    assert_eq!(buf.pos(), 18);
    assert!(buf.at_end());

    // Test byte array conversion
    let byte_arr = buf.to_byte_array();
    assert_eq!(byte_arr.as_str().unwrap(), "Hello Qt and Rust!");

    buf.close();
    assert!(!buf.is_open());
}

// =============================================================================
// 2. File & SaveFile
// =============================================================================

#[test]
fn test_file_iodevice_and_atomic_save_file() {
    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join("ferrokit_test_file_demo.txt");

    // Clean up if existing
    let _ = fs::remove_file(&file_path);

    // 1. Write file via QFile
    let mut file = File::new(&file_path);
    file.open_write().expect("open file for write");
    file.write_text("Line 1: Hello World\nLine 2: Second line\n")
        .expect("write text");
    file.close();
    assert!(!file.is_open());
    assert!(File::exists(&file_path));

    // 2. Read back via QFile
    let mut file_reader = File::new(&file_path);
    file_reader.open_read().expect("open file for read");
    let content = file_reader.read_all_text().expect("read all text");
    assert_eq!(content, "Line 1: Hello World\nLine 2: Second line\n");
    file_reader.close();

    // 3. Static helper methods
    let copy_path = temp_dir.join("ferrokit_test_file_demo_copy.txt");
    let _ = fs::remove_file(&copy_path);
    File::copy(&file_path, &copy_path).expect("copy file");
    assert!(File::exists(&copy_path));
    assert_eq!(File::read_text(&copy_path).unwrap(), content);
    File::remove(&copy_path).expect("remove copy");
    assert!(!File::exists(&copy_path));

    // 4. Atomic SaveFile with commit
    let target_save = temp_dir.join("ferrokit_test_savefile_target.txt");
    let _ = fs::remove_file(&target_save);

    // Initial write
    File::write_text_file(&target_save, "Original Content").expect("write original");

    // Start SaveFile
    let mut save_file = SaveFile::new(&target_save);
    save_file
        .open(OpenMode::WRITE_ONLY | OpenMode::TRUNCATE)
        .expect("open save file");
    save_file
        .write_all(b"New Atomic Content")
        .expect("write save file");
    save_file.commit().expect("commit save file");

    assert_eq!(File::read_text(&target_save).unwrap(), "New Atomic Content");

    // 5. Atomic SaveFile rollback when cancelled / dropped before commit
    {
        let mut uncommitted = SaveFile::new(&target_save);
        uncommitted
            .open(OpenMode::WRITE_ONLY | OpenMode::TRUNCATE)
            .expect("open");
        uncommitted.write_all(b"Corrupted Data").expect("write");
        // Dropping uncommitted here without commit()
    }
    // Target file MUST retain previous content!
    assert_eq!(File::read_text(&target_save).unwrap(), "New Atomic Content");

    // Clean up
    let _ = File::remove(&file_path);
    let _ = File::remove(&target_save);
}

// =============================================================================
// 3. Path Manipulation Utilities
// =============================================================================

#[test]
fn test_path_normalization_and_relative() {
    // clean_path
    assert_eq!(clean_path("a/b/../c/./d//e"), "a/c/d/e");
    assert_eq!(clean_path("/usr/local/../bin/"), "/usr/bin");
    assert_eq!(
        clean_path(r"C:\Windows\System32\..\Temp"),
        "C:/Windows/Temp"
    );

    // Separators
    let unix_style = "path/to/my/file.txt";
    let native = to_native_separators(unix_style);
    #[cfg(windows)]
    assert_eq!(native, r"path\to\my\file.txt");
    assert_eq!(from_native_separators(&native), unix_style);

    // Relative & Absolute
    assert!(is_relative("folder/sub"));
    assert!(is_relative("./relative.txt"));
    #[cfg(windows)]
    assert!(is_absolute(r"C:\Program Files"));
    assert!(is_absolute("/home/user"));

    // Relative path computation
    assert_eq!(relative_path("a/b/c", "a/b/d/e.txt"), "../d/e.txt");
    assert_eq!(relative_path("a/b", "a/b/c/d.txt"), "c/d.txt");

    // Component extractions
    let p = "/home/user/archive.tar.gz";
    assert_eq!(file_name(p), "archive.tar.gz");
    assert_eq!(base_name(p), "archive");
    assert_eq!(complete_base_name(p), "archive.tar");
    assert_eq!(suffix(p), "gz");
    assert_eq!(complete_suffix(p), "tar.gz");
    assert_eq!(suffix("/home/user/.bashrc"), "bashrc");
    #[cfg(unix)]
    assert_eq!(clean_path("//usr/bin"), "/usr/bin");
}

// =============================================================================
// 4. Directory, FileInfo & DirIterator
// =============================================================================

#[test]
fn test_directory_fileinfo_and_iterator() {
    let temp = Directory::temp();
    assert!(temp.exists());

    let test_dir_name = format!("ferrokit_dir_test_{}", std::process::id());
    let sub_path = temp.path().join(&test_dir_name);
    let _ = fs::remove_dir_all(&sub_path);

    let dir = Directory::new(&sub_path);
    dir.mkpath("sub1/sub2").expect("create directory hierarchy");
    assert!(dir.exists());

    // Create files in dir
    let file1 = sub_path.join("file_b.txt");
    let file2 = sub_path.join("file_a.log");
    File::write_text_file(&file1, "bbb").unwrap();
    File::write_text_file(&file2, "aaaa").unwrap();

    // Entry list with sorting by Name
    let entries = dir.entry_list(DirFilter::FILES, SortFlag::NAME).unwrap();
    assert_eq!(entries.join(","), "file_a.log,file_b.txt");

    // Entry list with reverse sorting
    let rev_entries = dir
        .entry_list(DirFilter::FILES, SortFlag::NAME | SortFlag::REVERSED)
        .unwrap();
    assert_eq!(rev_entries.join(","), "file_b.txt,file_a.log");

    // FileInfo metadata checks
    let info = FileInfo::new(&file2);
    assert!(info.exists());
    assert!(info.is_file());
    assert!(!info.is_dir());
    assert_eq!(info.size(), 4);
    assert_eq!(info.file_name(), "file_a.log");
    assert_eq!(info.suffix(), "log");

    // DirIterator recursive traversal
    let it = DirIterator::new(&sub_path, DirFilter::ALL_ENTRIES, true);
    let mut found_count = 0;
    for path in it {
        if path.is_file() {
            found_count += 1;
        }
    }
    assert!(found_count >= 2);

    // Clean up
    let _ = dir.rmpath(".");
    let _ = fs::remove_dir_all(&sub_path);
}

#[cfg(unix)]
#[test]
fn test_directory_symlinks_are_filtered_and_not_recursed() {
    use std::os::unix::fs::symlink;

    let root = Directory::temp()
        .path()
        .join(format!("ferrokit_symlink_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("child")).unwrap();
    fs::write(root.join("child/file.txt"), b"data").unwrap();
    symlink(&root, root.join("child/loop")).unwrap();
    symlink(root.join("child"), root.join("linked_child")).unwrap();

    let no_links = Directory::new(&root)
        .entry_info_list(DirFilter::ALL_ENTRIES | DirFilter::NO_SYM_LINKS, SortFlag::NAME)
        .unwrap();
    assert!(no_links.iter().all(|entry| !entry.is_symlink()));

    let paths: Vec<_> = DirIterator::new(&root, DirFilter::ALL_ENTRIES, true).collect();
    assert!(paths.len() < 10, "symlinked directory caused recursive traversal");
    let filtered: Vec<_> = DirIterator::new(
        &root,
        DirFilter::ALL_ENTRIES | DirFilter::NO_SYM_LINKS,
        true,
    )
    .collect();
    assert!(filtered.iter().all(|path| !path.is_symlink()));
    fs::remove_dir_all(root).unwrap();
}

// =============================================================================
// 5. Resource (QResource / qrc) Virtual File System
// =============================================================================

#[test]
fn test_resource_virtual_file_system() {
    // Clear registry before test
    Resource::clear();

    // Register embedded resources
    Resource::register_text(":/style/theme.css", "body { background: #1e1e1e; }");
    Resource::register_bytes(":/icons/app.png", &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A]);
    Resource::register_text(":/style/dark.qss", "QWidget { color: white; }");

    // Existence checks with both :/ and qrc:/
    assert!(Resource::exists(":/style/theme.css"));
    assert!(Resource::exists("qrc:/style/theme.css"));
    assert!(Resource::exists(":/icons/app.png"));
    assert!(!Resource::exists(":/missing/file.txt"));

    // Content retrieval
    let text = Resource::read_text(":/style/theme.css").unwrap();
    assert_eq!(text, "body { background: #1e1e1e; }");

    let bytes = Resource::read_bytes(":/icons/app.png").unwrap();
    assert_eq!(&bytes.as_bytes()[..4], &[0x89, 0x50, 0x4E, 0x47]);

    // Children listing
    let root_children = Resource::children(":/");
    assert!(root_children.contains(&"style".to_string()));
    assert!(root_children.contains(&"icons".to_string()));

    let style_children = Resource::children(":/style");
    assert_eq!(
        style_children,
        vec!["dark.qss".to_string(), "theme.css".to_string()]
    );

    // Reading via ResourceFile (IODevice)
    let mut res_file = ResourceFile::new(":/style/theme.css");
    res_file.open().expect("open resource file");
    assert!(res_file.is_open());
    assert!(res_file.is_readable());
    assert!(!res_file.is_writable());

    let mut buf = [0u8; 4];
    res_file.read(&mut buf).unwrap();
    assert_eq!(&buf, b"body");
    res_file.close();

    // Unregister
    assert!(Resource::unregister(":/style/theme.css"));
    assert!(!Resource::exists(":/style/theme.css"));
}

// =============================================================================
// 6. StandardPaths Resolution
// =============================================================================

#[test]
fn test_standard_paths_resolution() {
    let temp_dir = StandardPaths::writable_location(StandardLocation::TempLocation);
    assert!(temp_dir.is_dir());

    let home_dir = StandardPaths::writable_location(StandardLocation::HomeLocation);
    assert!(home_dir.is_dir());

    let appdata = StandardPaths::writable_location(StandardLocation::AppDataLocation);
    assert!(!appdata.as_os_str().is_empty());

    // find_executable
    #[cfg(windows)]
    {
        let cmd = StandardPaths::find_executable("cmd");
        assert!(cmd.is_some(), "cmd executable should be found in PATH");
    }
    #[cfg(not(windows))]
    {
        let sh = StandardPaths::find_executable("sh");
        assert!(sh.is_some(), "sh executable should be found in PATH");
    }
}

// =============================================================================
// 7. Url & UrlQuery Manipulation
// =============================================================================

#[test]
fn test_url_and_url_query_manipulation() {
    // 1. UrlQuery parsing & serialization
    let mut q = UrlQuery::from_query_string("name=ferrokit&version=1.0&topic=gui%20core");
    assert_eq!(q.query_item_value("name"), Some("ferrokit"));
    assert_eq!(q.query_item_value("version"), Some("1.0"));
    assert_eq!(q.query_item_value("topic"), Some("gui core"));

    q.add_query_item("author", "omp");
    assert!(q.has_query_item("author"));
    assert_eq!(q.query_item_value("author"), Some("omp"));

    q.remove_query_item("version");
    assert!(!q.has_query_item("version"));

    // 2. Url with query items
    let mut url = Url::new("https://github.com/ferrokit/framework?lang=rust");
    assert_eq!(url.query(), Some("lang=rust"));

    let mut query = url.query_items();
    query.add_query_item("status", "active");
    url.set_query_items(&query);
    assert!(url.as_str().contains("lang=rust"));
    assert!(url.as_str().contains("status=active"));

    // 3. Local file URL conversion. Drive letters and `\` are Windows path syntax: Qt converts
    // them only on Windows (qurl.cpp:3293-3295, 1545-1547), so each platform round-trips a
    // native absolute path.
    #[cfg(windows)]
    let local_path = PathBuf::from(r"C:\Users\test\document.txt");
    #[cfg(not(windows))]
    let local_path = PathBuf::from("/Users/test/document.txt");
    let file_url = Url::from_local_file(&local_path);
    assert!(file_url.as_str().starts_with("file://"));

    let extracted = file_url.to_local_file().expect("extract local file");
    assert_eq!(extracted, local_path);
}

// =============================================================================
// 8. Process Lifecycle & Execution
// =============================================================================

#[test]
fn test_process_lifecycle_and_execution() {
    let mut proc = Process::new();

    #[cfg(windows)]
    {
        proc.set_program("cmd.exe");
        proc.set_arguments(vec![
            "/C".to_string(),
            "echo".to_string(),
            "FERROKIT_PROCESS_OK".to_string(),
        ]);
    }
    #[cfg(not(windows))]
    {
        proc.set_program("sh");
        proc.set_arguments(vec!["-c".to_string(), "echo FERROKIT_PROCESS_OK".to_string()]);
    }

    proc.start().expect("start process");
    assert_eq!(proc.state(), ProcessState::Running);
    assert!(proc.process_id().is_some());

    let finished = proc.wait_for_finished(Some(Duration::from_secs(5)));
    assert!(finished);
    assert_eq!(proc.state(), ProcessState::NotRunning);
    assert_eq!(proc.exit_code(), Some(0));
    assert_eq!(proc.exit_status(), Some(ExitStatus::NormalExit));

    let stdout = String::from_utf8_lossy(&proc.read_all_stdout()).to_string();
    assert!(stdout.contains("FERROKIT_PROCESS_OK"));
    set_shell(&mut proc, "exit 7");
    proc.start().expect("start non-zero process");
    assert!(proc.wait_for_finished(Some(Duration::from_secs(5))));
    assert_eq!(proc.exit_code(), Some(7));
    assert_eq!(proc.exit_status(), Some(ExitStatus::NormalExit));
}

// =============================================================================
// 9. TemporaryFile & TemporaryDir Auto-Cleanup
// =============================================================================

#[test]
fn test_temporary_file_and_dir_auto_cleanup() {
    let file_path: PathBuf;
    {
        let mut temp_file = TemporaryFile::new().expect("create temp file");
        file_path = temp_file.file_path().to_path_buf();
        assert!(file_path.is_file());

        temp_file
            .write_all(b"Temporary data")
            .expect("write temp data");
        // temp_file dropped here with auto_remove = true
    }
    assert!(
        !file_path.exists(),
        "Temporary file must be deleted on drop"
    );

    let dir_path: PathBuf;
    {
        let temp_dir = TemporaryDir::new().expect("create temp dir");
        dir_path = temp_dir.path().to_path_buf();
        assert!(dir_path.is_dir());

        let inner_file = dir_path.join("inner.txt");
        File::write_text_file(&inner_file, "Inner data").unwrap();
        // temp_dir dropped here with auto_remove = true
    }
    assert!(
        !dir_path.exists(),
        "Temporary directory must be deleted on drop"
    );
}

#[test]
fn test_temporary_paths_are_distinct_and_exclusive() {
    let first = TemporaryFile::with_prefix_and_suffix("ferrokit_exclusive", ".tmp").unwrap();
    let second = TemporaryFile::with_prefix_and_suffix("ferrokit_exclusive", ".tmp").unwrap();
    assert_ne!(first.file_path(), second.file_path());
    assert!(first.file_path().exists());
    assert!(second.file_path().exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(first.file_path()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let first_dir = TemporaryDir::with_prefix("ferrokit_exclusive_dir").unwrap();
    let second_dir = TemporaryDir::with_prefix("ferrokit_exclusive_dir").unwrap();
    assert_ne!(first_dir.path(), second_dir.path());
    assert!(first_dir.path().is_dir());
    assert!(second_dir.path().is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(first_dir.path()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}

// =============================================================================
// 10. LockFile Concurrency & Stale Detection
// =============================================================================

#[test]
fn test_lock_file_advisory_locking() {
    let temp_dir = std::env::temp_dir();
    let lock_path = temp_dir.join(format!("ferrokit_test_lock_{}.lock", std::process::id()));
    let _ = fs::remove_file(&lock_path);

    let mut lock1 = LockFile::new(&lock_path);
    assert!(!lock1.is_locked());

    let ok1 = lock1.try_lock_now().expect("try lock 1");
    assert!(ok1);
    assert!(lock1.is_locked());
    assert!(lock_path.is_file());

    // Lock info
    let info = lock1.get_lock_info().expect("get lock info");
    assert_eq!(info.0, std::process::id());

    // Second lock attempt from another instance MUST fail
    let mut lock2 = LockFile::new(&lock_path);
    let ok2 = lock2.try_lock(Duration::from_millis(100));
    assert!(ok2.is_err() || ok2 == Ok(false));

    // Release first lock
    lock1.unlock();
    assert!(!lock1.is_locked());
    assert!(!lock_path.exists());

    // Now second lock can acquire
    let ok2_after = lock2.try_lock_now().expect("try lock 2 after release");
    assert!(ok2_after);
    assert!(lock2.is_locked());

    lock2.unlock();
}

// =============================================================================
// 11. StorageInfo Volume Query
// =============================================================================

#[test]
fn test_storage_info_queries() {
    let root = StorageInfo::root();
    assert!(root.is_valid(), "Root storage volume must be valid");
    assert!(root.is_ready(), "Root storage volume must be ready");
    assert!(
        root.bytes_total() > 0,
        "Root storage total bytes must be > 0"
    );
    assert!(
        root.bytes_available() > 0,
        "Root storage available bytes must be > 0"
    );
    assert!(
        !root.file_system_type().is_empty(),
        "Root file system type must not be empty"
    );

    let volumes = StorageInfo::mounted_volumes();
    assert!(
        !volumes.is_empty(),
        "At least one storage volume must be detected"
    );
    assert!(volumes.iter().any(|v| v.is_valid()));
}
#[test]
fn test_process_spawn_failure_restores_not_running() {
    let mut proc = Process::new();
    proc.set_program("ferrokit-command-that-does-not-exist");
    assert!(proc.start().is_err());
    assert_eq!(proc.state(), ProcessState::NotRunning);
}
#[test]
fn test_buffer_rejects_unrepresentable_seek_and_write() {
    let mut buffer = Buffer::new();
    buffer.open(OpenMode::READ_WRITE).unwrap();
    assert!(IODevice::seek(&mut buffer, u64::MAX).is_err());
    assert!(std::io::Seek::seek(&mut buffer, std::io::SeekFrom::Start(u64::MAX)).is_err());
    assert!(buffer.data().is_empty());
}

fn set_shell(proc: &mut Process, script: &str) {
    #[cfg(windows)]
    {
        proc.set_program("cmd.exe");
        proc.set_arguments(vec!["/C".to_string(), script.to_string()]);
    }
    #[cfg(not(windows))]
    {
        proc.set_program("sh");
        proc.set_arguments(vec!["-c".to_string(), script.to_string()]);
    }
}

fn long_running_script() -> &'static str {
    #[cfg(windows)]
    {
        "ping -n 30 127.0.0.1 >nul"
    }
    #[cfg(not(windows))]
    {
        "sleep 30"
    }
}

#[test]
fn process_start_failure_keeps_not_running() {
    let mut proc = Process::new();
    proc.set_program("ferrokit-no-such-program-xyz");
    assert!(proc.start().is_err());
    assert_eq!(proc.state(), ProcessState::NotRunning);
}

#[test]
fn process_large_output_is_drained_without_deadlock() {
    let mut proc = Process::new();
    #[cfg(windows)]
    {
        proc.set_program("cmd.exe");
        proc.set_arguments(vec![
            "/C".into(),
            "for /L %i in (1,1,20000) do @echo 0123456789012345678901234567890123456789".into(),
        ]);
    }
    #[cfg(not(windows))]
    {
        proc.set_program("sh");
        proc.set_arguments(vec![
            "-c".into(),
            "i=0; while [ $i -lt 20000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done".into(),
        ]);
    }
    proc.start().unwrap();
    assert!(proc.wait_for_finished(Some(Duration::from_secs(60))));
    assert!(proc.read_all_stdout().len() > 64 * 1024);
}

#[test]
fn process_merged_channels_put_stderr_on_stdout_in_write_order() {
    let mut proc = Process::new();
    proc.set_channel_mode(ProcessChannelMode::MergedChannels);
    #[cfg(windows)]
    set_shell(&mut proc, "echo out& echo err 1>&2");
    #[cfg(not(windows))]
    set_shell(&mut proc, "echo out; echo err >&2");
    proc.start().unwrap();
    assert!(proc.wait_for_finished(Some(Duration::from_secs(30))));
    let merged = String::from_utf8_lossy(&proc.read_all_stdout()).into_owned();
    let lines: Vec<&str> = merged.lines().map(str::trim).collect();
    assert_eq!(lines, ["out", "err"]);
    assert!(proc.read_all_stderr().is_empty());
}

#[test]
fn process_wait_times_out_and_kill_reaps() {
    let mut proc = Process::new();
    set_shell(&mut proc, long_running_script());
    proc.start().unwrap();
    assert!(!proc.wait_for_finished(Some(Duration::from_millis(50))));
    assert_eq!(proc.state(), ProcessState::Running);
    // wait_for_finished must not close stdin.
    assert!(proc.write_stdin(b"x").is_ok() || cfg!(windows));
    proc.kill().unwrap();
    assert_eq!(proc.state(), ProcessState::NotRunning);
    assert_eq!(proc.exit_status(), Some(ExitStatus::CrashExit));
    assert!(proc.process_id().is_none());
}

#[cfg(unix)]
#[test]
fn process_signal_death_is_crash_exit() {
    let mut proc = Process::new();
    set_shell(&mut proc, "kill -9 $$");
    proc.start().unwrap();
    assert!(proc.wait_for_finished(Some(Duration::from_secs(10))));
    assert_eq!(proc.exit_status(), Some(ExitStatus::CrashExit));
}

#[cfg(unix)]
#[test]
fn process_wait_honours_timeout_when_grandchild_holds_pipe() {
    let mut proc = Process::new();
    set_shell(&mut proc, "sleep 20 & exit 0");
    proc.start().unwrap();
    let begin = std::time::Instant::now();
    assert!(proc.wait_for_finished(Some(Duration::from_millis(500))));
    assert!(begin.elapsed() < Duration::from_secs(10));
    assert_eq!(proc.exit_code(), Some(0));
}

#[test]
fn save_file_drop_without_commit_removes_temp_and_keeps_target() {
    let dir = TemporaryDir::new().unwrap();
    let target = dir.path().join("t.txt");
    File::write_text_file(&target, "old").unwrap();
    {
        let mut save = SaveFile::new(&target);
        save.open(OpenMode::WRITE_ONLY).unwrap();
        save.write(b"new").unwrap();
    }
    assert_eq!(File::read_text(&target).unwrap(), "old");
    let leftovers = fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(leftovers, 1, "temp file must be removed on drop");
}

#[cfg(unix)]
#[test]
fn save_file_new_target_uses_default_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TemporaryDir::new().unwrap();
    let probe = dir.path().join("probe");
    fs::write(&probe, b"").unwrap();
    let expected = fs::metadata(&probe).unwrap().permissions().mode() & 0o7777;
    let target = dir.path().join("new.txt");
    let mut save = SaveFile::new(&target);
    save.open(OpenMode::WRITE_ONLY).unwrap();
    save.write(b"x").unwrap();
    save.commit().unwrap();
    assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o7777, expected);
}

#[test]
fn lock_file_stale_reclaim_gives_exactly_one_owner() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    for _ in 0..20 {
        let dir = TemporaryDir::new().unwrap();
        let path = dir.path().join("app.lock");
        // Stale: owner pid cannot be alive.
        fs::write(&path, "PID=4294967294\nAPP=dead\nTIME=0\n").unwrap();
        let holders = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let done = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let (path, holders, barrier, done) =
                (path.clone(), holders.clone(), barrier.clone(), done.clone());
            handles.push(std::thread::spawn(move || {
                let mut lock = LockFile::new(&path);
                barrier.wait();
                let got = lock.try_lock_now().unwrap_or(false);
                if got {
                    holders.fetch_add(1, Ordering::SeqCst);
                }
                done.wait();
                drop(lock);
            }));
        }
        done.wait();
        assert_eq!(holders.load(Ordering::SeqCst), 1);
        for h in handles {
            h.join().unwrap();
        }
    }
}

#[test]
fn settings_roundtrip_plain_strings_and_escapes() {
    use ferrokit_core::settings::Settings;
    use ferrokit_core::variant::Variant;
    let dir = TemporaryDir::new().unwrap();
    let path = dir.path().join("s.ini");
    {
        let mut s = Settings::new(&path);
        s.set_value("name", "hello world");
        s.set_value("multi", "a\nb=c\r\nd");
        s.set_value("looks_num", "123");
        s.set_value("padded", "  x  ");
        s.set_value("we=ird\nkey", "v");
        s.set_value("n", 5i64);
        s.sync().unwrap();
    }
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("name=hello world"), "{text}");
    assert_eq!(text.lines().filter(|l| l.contains("=v")).count(), 1);
    let s = Settings::new(&path);
    assert_eq!(s.value("name"), Some(Variant::String("hello world".into())));
    assert_eq!(s.value("multi"), Some(Variant::String("a\nb=c\r\nd".into())));
    assert_eq!(s.value("looks_num"), Some(Variant::String("123".into())));
    assert_eq!(s.value("padded"), Some(Variant::String("  x  ".into())));
    assert_eq!(s.value("we=ird\nkey"), Some(Variant::String("v".into())));
    assert_eq!(s.value("n"), Some(Variant::I64(5)));
}

#[test]
fn settings_legacy_unmarked_file_loads_verbatim_and_section_backslash_survives() {
    use ferrokit_core::settings::Settings;
    use ferrokit_core::variant::Variant;
    let dir = TemporaryDir::new().unwrap();
    let path = dir.path().join("old.ini");
    fs::write(&path, "p=C:\\new\\temp\nunc=\\\\server\\share\n").unwrap();
    {
        let mut s = Settings::new(&path);
        assert_eq!(s.value("p"), Some(Variant::String("C:\\new\\temp".into())));
        assert_eq!(s.value("unc"), Some(Variant::String("\\\\server\\share".into())));
        s.set_value("x", "1 ");
        s.sync().unwrap();
    }
    let s = Settings::new(&path);
    assert_eq!(s.value("p"), Some(Variant::String("C:\\new\\temp".into())));
    assert_eq!(s.value("unc"), Some(Variant::String("\\\\server\\share".into())));

    let path2 = dir.path().join("new.ini");
    {
        let mut s = Settings::new(&path2);
        s.set_value("sec\\/k", "v");
        s.sync().unwrap();
    }
    let s = Settings::new(&path2);
    assert_eq!(s.value("sec\\/k"), Some(Variant::String("v".into())));
}

#[test]
fn lock_file_empty_reclaim_guard_is_reclaimed_once_stale() {
    let dir = TemporaryDir::new().unwrap();
    let path = dir.path().join("app.lock");
    fs::write(&path, "PID=4294967294\nAPP=dead\nTIME=0\n").unwrap();
    let mut guard = path.as_os_str().to_owned();
    guard.push(".reclaim");
    fs::write(&guard, "").unwrap();
    let mut lock = LockFile::new(&path);
    lock.set_stale_lock_time(Duration::ZERO);
    assert_eq!(lock.try_lock(Duration::from_secs(10)), Ok(true));
}

#[test]
fn settings_roundtrip_edge_whitespace_in_keys_and_sections() {
    use ferrokit_core::settings::Settings;
    use ferrokit_core::variant::Variant;
    let dir = TemporaryDir::new().unwrap();
    let path = dir.path().join("ws.ini");
    {
        let mut s = Settings::new(&path);
        s.set_value(" lead", "1");
        s.set_value("trail ", "2");
        s.set_value(" sec /\tkey\t", "3");
        s.set_value("mid dle", "4");
        s.sync().unwrap();
    }
    let s = Settings::new(&path);
    assert_eq!(s.value(" lead"), Some(Variant::String("1".into())));
    assert_eq!(s.value("trail "), Some(Variant::String("2".into())));
    assert_eq!(s.value(" sec /\tkey\t"), Some(Variant::String("3".into())));
    assert_eq!(s.value("mid dle"), Some(Variant::String("4".into())));
}
