# ferrokit

A Rust implementation of a subset of Qt®: the object model, the event loop, signals and slots,
painting, text layout, style sheets, widgets and layouts, and a Win32 platform layer.

> **Status: 0.x, unstable API, not published to crates.io.**
> Windows x86_64 is the only supported platform and the only one that has been run on a real
> desktop. The macOS and Linux back ends exist but are experimental: CI checks that they compile
> and runs the tests that can run there. Nothing has been verified on a real macOS or Linux
> desktop.

## Why

ferrokit came out of porting a PySide6 desktop application to Rust. The two versions ran on
different UI stacks, so keeping their behaviour in step was hard. Implementing the parts of Qt that
the application relies on, with Qt's own semantics, lets code written against Qt's model be ported
without rediscovering how each widget and layout behaves.

## Crates

| crate | contents |
|---|---|
| `ferrokit-core` | object model (parent and child objects, properties, meta objects), events, event loop, signals and slots, timers, threads, I/O helpers |
| `ferrokit-gui` | geometry, colours, painter (software rasteriser), text shaping and fonts, images |
| `ferrokit-platform` | native windows, screens, cursor, clipboard, tray icons, menus, dialogs; Win32 first, other back ends experimental |
| `ferrokit-widgets` | widgets, layouts, style sheets (QSS), tool tips, menus |
| `ferrokit-derive` | derive macro for the object model |

## How close to Qt is it?

Behaviour was aligned with Qt 6.11 by comparing against it, mostly through PySide6 6.11.2, and the
tests pin the cases that were compared. It is a subset and not a drop-in replacement: many
differences remain, and the API follows Qt's semantics rather than its C++ signatures.

## Building and testing

```
cargo build
cargo test --workspace
```

- Tests share process-wide state, so `.cargo/config.toml` runs them one thread at a time.
- Many tests use Windows system fonts (Segoe UI, Arial, Microsoft JhengHei and others). On Linux,
  CI installs the Microsoft core fonts and Noto CJK to get the same families.
- `tools/run_test_binaries.py` runs every test binary on its own with a time limit.
- Developed and tested with the current stable Rust toolchain (rustc 1.98.1 at the time of writing);
  CI uses the toolchain that GitHub's runner images provide and prints its version in the log. No
  minimum supported Rust version is tested or promised. The crates use edition 2021, and the locked
  dependency `unicode-segmentation` alone needs Rust 1.85.

## Environment variables

| variable | effect |
|---|---|
| `FERROKIT_ENABLE_DCOMP=1` | use the DirectComposition presenter on Windows (opt-in; it falls back to the layered-window presenter on failure) |
| `FERROKIT_RESIZE_DEBUG=1` or a path | log interactive-resize diagnostics to stderr and a file |
| `FERROKIT_STARTUP_TRACE=1` or a path | log startup timing |
| `FERROKIT_FONT_ENGINE=outline` | force the generic outline font backend |
| `FERROKIT_HEADLESS` | macOS back end only: treat the session as headless (`CI`, `GITHUB_ACTIONS` and `QT_QPA_PLATFORM=offscreen` do the same) |

## License

LGPL-3.0-only, see [LICENSE](LICENSE). ferrokit is derived from the design and behaviour of Qt, see
[NOTICE](NOTICE) for the attribution. Qt is a registered trademark of The Qt Company Ltd. and its
subsidiaries; this project is independent and is not affiliated with them.

Every Rust source file starts with an `SPDX-License-Identifier: LGPL-3.0-only` line. Unless you
state otherwise, a contribution you submit is licensed under LGPL-3.0-only, with no additional terms.

Rust links crates statically by default. If you distribute a closed-source program that includes
ferrokit, section 4 of the LGPL applies to you, in particular its requirements about letting users
relink the program with a modified version of the library. This is not legal advice.
