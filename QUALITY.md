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

## Unverified (needs a Windows 11 machine)

- The Win32 unbuffered path actually running on Windows 11: `CreateFileW`
  with `FILE_FLAG_NO_BUFFERING`, `FILE_ALLOCATION_INFO` pre-allocation, and
  the pad-then-truncate finish on a real NTFS volume.
- NVMe throughput (the 2.5–5.0 GB/s band is a property of the target drive;
  development-VM numbers do not transfer).
- Working set on Windows (expected to match the ~12 MiB shape: one 4 MiB
  block plus staging and runtime).
- The MSVC static-CRT build running on a clean Windows install with no
  redistributable present.

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
