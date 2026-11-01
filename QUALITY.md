# Quality — TextSwell 0.1.0

Gate run by the integrator on the final tree before publish. Labels are
honest: **Verified** means the evidence below was produced in this
environment; **Unverified** means only the target machine can settle it.

## Verified

- **Unit/integration tests: 17/17 pass** (`cargo test --release`, Linux host)
  covering parsers, PRNG determinism, SHA-256 FIPS vectors, every profile's
  shape contract, needle placement across uneven block boundaries, null-rate
  distribution, `--line-len` enforcement, and buffer alignment.
- **Clippy: clean** (`cargo clippy --release --all-targets`, no warnings).
- **Windows target builds**: `cargo build --release --target
  x86_64-pc-windows-gnu` produces `largetxt-gen.exe` (374,272 bytes)
  importing only system DLLs (KERNEL32, ntdll, msvcrt, api-ms synch).
- **End-to-end smoke (Linux fallback writer, same output semantics)**:
  - All six profiles produce files of the *exact* requested size, including
    the non-sector-multiple size 1,234,567 with unbuffered padding active.
  - Determinism: two runs with the same seed produce identical SHA-256.
  - The printed SHA-256 matches an independent `sha256sum` / Python
    `hashlib` digest of the written file.
  - Needle index: every recorded byte offset in a 3 MiB `needle-bench` file
    (needles every 1 MiB) contains the needle, and every recorded line
    number matches an independent recount of `\n` bytes before the offset.
  - `--null-rate 25` yields 25.04% NUL bytes over 1 MiB.
  - `--eol lf` yields zero CR bytes; `--line-len 80:120` yields dense-code
    lines of exactly 80–120 chars (min 80, max 120, avg 91.2 over 21,460
    lines) and ragged-csv lines within a custom 50:200 range.
  - `infinite-line`: zero CR/LF bytes in the whole output file.
  - **Constant memory: peak child RSS 11.5 MiB while generating 1 GiB**
    (under the 16 MB budget), at ~195 MiB/s on this VM's virtual disk with
    generation + SHA-256 included.
  - Usage errors exit 1 with a clear message; `--help`/`--version` exit 0.

## Field-verified on Windows 11 (2026-11-01, owner's machine)

- **Full unbuffered run completed**: `needle-bench`, 10 GiB,
  `--unbuffered true`, default seed — the `CreateFileW`
  `FILE_FLAG_NO_BUFFERING` path, pre-allocation, and pad-then-truncate
  finish all worked on a real NTFS volume. Final size exactly
  10,737,418,240 bytes, elapsed 1:17, SHA-256
  `7bd8222a5797d404630b5e1a9fcd6fb008a7ed72b5b6981076fb16deccf48e24`,
  **21 needle placements** recorded (offsets 0 through 20 × 500 MiB) with
  the `velo-10g.txt.index` sidecar written.
- **Throughput honesty**: average 132.2 MB/s on that run. The line-shaped
  profiles are bound by single-threaded generation + SHA-256, not by the
  disk — on a fast NVMe drive they will not reach raw write bandwidth.
  The stream profiles (`infinite-line`, `dirty-hex`) carry far less
  per-byte work and are the right choice for a pure bandwidth measure;
  their Windows numbers are still unmeasured.

## Unverified

- Working set on Windows (expected to match the ~12 MiB shape: one 4 MiB
  block plus staging and runtime).
- Stream-profile throughput on a real NVMe drive.
- The MSVC static-CRT build running on a clean Windows install with no
  redistributable present (the field run used the MinGW build).

## Scorecard (Elite prompt §17 shape)

| Dimension | Score (0–5) | Evidence |
|---|---|---|
| Correctness | 5 | 17/17 tests; byte-exact sizes; checksum + index independently re-verified |
| Performance design | 4 | Unbuffered pre-allocated aligned writes; allocation-free generation. Held at 4 until NVMe numbers exist on Windows |
| Memory | 5 | 11.5 MiB peak RSS at 1 GiB output |
| Robustness | 4 | Partial-write loops, allocation-failure tolerance, hard stops in row generation; no fuzz corpus yet |
| Clarity | 5 | Single file, sectioned and commented; every flag documented in `--help` and README |
| Platform honesty | 5 | Windows-only claims labelled Unverified rather than asserted |

No dimension below 3, no Fail. The Windows field checks above are the
follow-up list, not hidden gaps.
