# Fuzz harnesses

[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) targets for the parsers that
read untrusted bytes. This crate is excluded from the workspace and from CI; it needs a nightly
toolchain and a Unix host.

| Target        | Exercises                                                                                       | Properties checked                                                  |
|---------------|-------------------------------------------------------------------------------------------------|---------------------------------------------------------------------|
| `json_parse`  | `ferrokit_core::json::from_slice`                                                               | no panic; parse -> serialize (compact, indented) -> parse is stable |
| `data_stream` | `DataStreamReader` typed reads, `DataStreamWriter` typed writes                                 | no panic or oversized allocation; write -> read returns the input   |
| `variant`     | `Variant::from_bytes` / `to_bytes`                                                              | no panic; deserialize -> serialize -> deserialize is stable; generated variants round-trip |
| `qcssparser`  | `QCssStyleSheet::parse`, `QCssParser::parse`, `parse_length`, `parse_color`                     | no panic, hang or memory blow-up                                    |
| `dbus_frame`  | `DbusMessage::frame_len` / `from_bytes`, `DbusConnection::read_incoming` over a scripted bus    | no panic; decode -> encode -> decode is stable; chunked reads are fine |

## Running

```sh
cargo install cargo-fuzz
cd <repo root>

# The first corpus directory receives new inputs (git-ignored); the seeds are read-only extras.
cargo +nightly fuzz run json_parse fuzz/corpus/json_parse fuzz/seeds/json_parse

# A long unattended run that keeps going after a finding and flags hangs after 10 s:
cargo +nightly fuzz run dbus_frame fuzz/corpus/dbus_frame fuzz/seeds/dbus_frame -- \
    -max_total_time=3600 -timeout=10 -fork=2 -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1
```

Findings are written to `fuzz/artifacts/<target>/`. Minimise one with
`cargo +nightly fuzz tmin <target> <artifact>`, replay it with
`cargo +nightly fuzz run <target> <artifact>`, and turn the minimised input into a regression unit
test in the crate that owns the code before fixing it. Artifacts are not committed.

`fuzz/seeds/` holds a few small hand-written inputs per target (well under 1 MB in total); keep it
that way and do not commit generated corpora.
