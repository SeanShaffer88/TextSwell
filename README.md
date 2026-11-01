# TextSwell — `largetxt-gen`

A zero-dependency Windows command-line tool that generates massive test files — multi-gigabyte to multi-terabyte — engineered to stress-test the limits of a high-performance large-file text/hex editor.

## TL;DR

You run one command and it writes a huge file as fast as your drive can take it, using almost no memory. The file is not random noise: you pick a shape — realistic source code, one endless line with no line breaks, messy spreadsheet rows, binary junk mixed with text, dense Unicode, or text with hidden target words planted at exact spots — so you can test how an editor handles that exact shape. Same settings and same seed always make the exact same file, and it prints a checksum at the end so you can prove it.

## Key features

- **Constant memory** — a bounded pool of 2 MiB write blocks (sized to the generator thread count) reused end to end. Generating a 100 GB file uses the same working set as a 500 MB file (measured resident set about 19 MiB with two generator threads).
- **Unbuffered, pre-allocated writes on Windows** — `CreateFileW` with `FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN | FILE_SHARE_READ`, NTFS pre-allocation through `SetFileInformationByHandle` (`FILE_ALLOCATION_INFO`) so the file lands in as few fragments as possible, and 4 KiB-aligned buffers allocated with `VirtualAlloc`.
- **Exact final size** — unbuffered writes must be whole sectors, so the last block is padded, written, and then cut back to the exact requested byte with `SetFilePointerEx` + `SetEndOfFile`. Ask for 1,234,567 bytes and the file is 1,234,567 bytes.
- **Deterministic** — an inlined Xoshiro256++ generator driven by `--seed`. Same flags, same bytes, same SHA-256, every run — and the stream profiles are generated in fixed segments so the bytes cannot change if an internal block size ever does.
- **Parallel generation, pipelined and hardware-hashed** — several generator threads produce blocks concurrently (each block is seeded independently, so any thread count produces identical bytes), while SHA-256 and writing run as their own pipeline stages; on CPUs with SHA extensions the checksum uses hardware SHA-NI rounds (byte-identical to the software path, proven by tests). `--threads <N>` controls the generator count (default: auto).
- **Built-in SHA-256** — the checksum of the written stream is printed at the end, computed while generating (no second read pass).
- **Live progress** — bytes done / total with a percentage bar, instant and average throughput, elapsed time, and ETA, refreshed every 100 ms using `QueryPerformanceCounter`.
- **Zero dependencies** — no crates at all. Only the Rust standard library plus direct `kernel32.dll` calls declared in the source.
- **Portable** — a single self-contained executable. The MSVC build links the C runtime statically; nothing to install.

## Profiles (`--profile`)

| Profile | What it generates | What it stresses |
|---|---|---|
| `dense-code` (default) | Realistic synthetic C++/Rust-style source lines — indentation, braces, declarations, calls, string literals, comments — averaging 40–120 chars, CRLF endings | Tokenizing, piece tables, line indexes |
| `infinite-line` | One uninterrupted stream of printable characters with **zero** `\r` or `\n` across the whole file | Line-offset tables, wrap calculators |
| `ragged-csv` | CSV/TSV rows with 1–16+ columns, quoted fields with escaped quotes, mixed CRLF/LF/CR endings, row lengths from 10 to 8,000+ chars | Ragged parsing, mixed-ending handling |
| `dirty-hex` | Raw random bytes laced with valid UTF-8 fragments, NUL runs, the invalid UTF-8 encoding of an orphaned surrogate (`ED A0 80`), truncated sequences, UTF-16 BOM fragments, and ANSI escapes | Binary-vs-text detection, hex view, validators |
| `unicode-stress` | Dense multi-byte UTF-8: CJK, kana, hangul, Arabic, Hebrew, Devanagari, emoji, combining marks and Zalgo stacks, U+202E right-to-left override | Glyph layout, font fallback, shaping caches |
| `needle-bench` | Predictable numbered filler lines with a target string stamped at exact byte intervals; writes a `<output>.index` sidecar with the byte offset and 1-based line number of every needle | Search correctness and speed |

## Requirements

- Windows 11, x86_64 (the shipped target). The generator, parser, and checksum also build and run on Linux through a `std::fs` fallback with identical output semantics, which is how the test suite runs anywhere.
- To build: a stable Rust toolchain. Nothing else — there are no dependencies to fetch.

## Install

Download `largetxt-gen.exe` from the Releases page and put it anywhere on your `PATH`, or build it yourself (below). It is a portable executable: no installer, no settings, no network use.

## Build

```text
cargo build --release
cargo build --release --target x86_64-pc-windows-msvc   # shipped Windows build (static CRT)
cargo test  --release                                    # full test suite
```

The binary is written to `target/release/largetxt-gen(.exe)`.

## How to use it

```text
largetxt-gen big.cpp -s 10G
```
Writes a 10 GiB `dense-code` file.

```text
largetxt-gen endless.txt -s 20G -p infinite-line
```
One 20 GiB line with no line break anywhere.

```text
largetxt-gen search.txt -s 5G -p needle-bench --needle "TARGET_STRING" --needle-every 500M
```
Writes `search.txt` plus `search.txt.index`:

```text
# largetxt-gen needle index v1
# needle: TARGET_STRING
# needle_every_bytes: 524288000
# byte_offset	line_number	needle
0	1	TARGET_STRING
524288000	4912345	TARGET_STRING
```

A test harness can read any listed offset, check the needle is really there, and compare its own search results line-for-line.

```text
largetxt-gen messy.csv -s 2G -p ragged-csv --eol mixed --seed 0xC0FFEE
largetxt-gen hex.bin   -s 1G -p dirty-hex --null-rate 5
largetxt-gen uni.txt   -s 1G -p unicode-stress --line-len 80:120
```

Sizes accept `K`, `M`, `G`, `T` suffixes (binary: `1G` = 1 GiB), an optional trailing `B` (`512MB`), plain byte counts (`4096`), and fractions (`1.5G`).

### All flags

| Flag | Meaning | Default |
|---|---|---|
| `<OUTPUT_PATH>` | File to create/overwrite | required |
| `-s, --size <SIZE>` | Total size | `1G` |
| `-p, --profile <PROFILE>` | One of the six profiles above | `dense-code` |
| `--line-len <MIN:MAX>` | Line-length range for line profiles; lines are padded/truncated to land inside it | per profile |
| `--eol <crlf\|lf\|cr\|mixed>` | Line terminator | `crlf` (`mixed` for `ragged-csv`) |
| `--needle <TEXT>` | Needle to stamp at intervals (any profile) | `TARGET_STRING` on `needle-bench` |
| `--needle-every <SIZE>` | Spacing between needles; must be at least the needle length | `500M` |
| `--null-rate <PCT>` | Percent of bytes forced to `0x00` (0.0–100.0). Applied before needle stamping, so needles stay findable | `0` |
| `--unbuffered <BOOL>` | `FILE_FLAG_NO_BUFFERING` on Windows | `true` |
| `--seed <HEX>` | 64-bit hex seed for repeatable output | `0x5EED5EED5EED5EED` |
| `--threads <N>` | Generator threads, 1–64 (0 = auto: cores − 2, max 8) | auto |
| `-h, --help` / `-V, --version` | Help / version | |

Exit codes: `0` success, `1` usage error, `2` I/O error.

## How it works

- A bounded pool of 2 MiB buffers, page-aligned via `VirtualAlloc`, cycles through the pipeline: generator worker threads fill buffers (each block from its own derived seed, so the file is identical no matter how many workers run), a sequencer restores block order and applies needle stamping and null forcing, a hashing thread feeds each block through SHA-256, and the writer thread writes it and returns it to the pool. The pool hands buffers out in block order, so an early block can never be starved by later blocks finishing first. Nothing is allocated per line after startup: line-shaped profiles build one line in a reusable 16 KiB+ staging buffer and copy out of it.
- The file is pre-allocated to its final (padded) size up front, so the filesystem can place it contiguously.
- Every block is fed through the built-in SHA-256 (hardware-accelerated when the CPU offers SHA extensions) as it flows to the writer, so the final checksum costs no extra pass.
- Needles are stamped over the generated stream at exact multiples of `--needle-every` (starting at offset 0), including needles that straddle a block boundary.

## Tests

`cargo test --release` runs the full suite: size/seed/line-length parsers, PRNG determinism, SHA-256 against the published FIPS test vectors (including the million-`a` vector fed in odd-sized chunks), per-profile shape checks (no newlines in `infinite-line`, ragged length spread in `ragged-csv`, invalid-byte patterns in `dirty-hex`, valid UTF-8 with right-to-left marks in `unicode-stress`), needle placement at exact offsets across uneven block boundaries, null-rate distribution, `--line-len` enforcement, and sector alignment of the write buffer.

## Limitations and known issues

- Throughput depends on the drive, the CPU, and the profile. Generation scales with `--threads` on multi-core machines; on a very fast drive a single hashing thread and the write path eventually bound the total. The stream profiles (`infinite-line`, `dirty-hex`) are the ones to use for pure write-bandwidth measurement. Peak NVMe rates (2.5–5.0 GB/s) are a property of the target machine and are not claimed from development-machine measurements.
- On non-Windows hosts the fallback writer cannot request unbuffered I/O; output bytes and sizes are identical, write behaviour is not.
- `--null-rate` costs one random draw per byte and slows generation while it is active.
- Very small `--needle-every` values on very large files produce very large `.index` sidecars (one row per needle).

## License

MIT — see `LICENSE`.
