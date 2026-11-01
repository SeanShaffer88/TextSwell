# TextSwell — Project Rules

Prompt in force: The Elite Software Master Prompt v1.1 · Annexes: A (Windows)
Purpose in one line: Generate massive, deterministically shaped test files at disk speed with constant memory, to stress large-file text/hex editors.
Frozen foundation: Single-file Rust binary (`src/main.rs`), zero external crates; Win32 only through raw `extern "system"` kernel32 declarations; the generator never allocates after startup.
Build: `cargo build --release` · Full gate: `cargo test --release` + `cargo clippy --release --all-targets` + `cargo build --release --target x86_64-pc-windows-gnu` · Test corpus: unit tests in `src/main.rs` §12 plus end-to-end smoke runs recorded in `QUALITY.md`.
Budgets: Peak RSS < 16 MiB at any output size; one 4 MiB write block; progress refresh no faster than every 100 ms; staging buffer sized from `--line-max` only.
Coverage floors: parsers, checksum, needle placement, and every profile's shape contract are covered by tests; new profiles or flags land with tests in the same change.
Special rules for this project: Output bytes must be identical for a given seed/profile/flags on every host (the fallback writer exists to prove it); needle byte offsets in the `.index` sidecar are a public contract and must stay exact, including across block boundaries; never claim Windows throughput or behaviour from non-Windows evidence — label it Unverified in `QUALITY.md`.
Known limitations live at: README "Limitations and known issues" · Decision log: CHANGELOG.md · QUALITY.md: repo root.
