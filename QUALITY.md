# Quality — TextSwell 0.3.3

## 0.3.3 sync-writer revert — verified (VM), field run owed

- **What changed**: the overlapped write path (0.3.1–0.3.2) is
  removed; the writer is 0.3.0's synchronous `WriteFile` loop again
  (same submit/reap accounting shape, writes complete at submit).
  EOF pre-sizing removed with it. Kept: SWAR newline counting,
  `--threads` auto cap 16, stage queues at 8.
- **Why**: two field runs put the overlapped versions at 299.1 and
  287.6 MB/s against the sync version's 640.1 MB/s, output SHA
  identical every time — the experiment had its answer.
- **Byte-identity**: A/B vs the 0.3.0 binary at 37 MiB, 10/10 cases
  identical (files + needle indexes).
- **Gate**: 22/22 tests, clippy clean host + windows-gnu, fmt clean.
- **Field verification owed**: the owner's 10 GiB needle-bench
  should land back at ~0:16 / ~640 MB/s with SHA `b6a6ca0a…`.

# Quality — TextSwell 0.3.2

## 0.3.2 regression fix — verified (VM), field run owed

- **What changed**: Windows create now pre-sizes the file's
  end-of-file to the padded total (not just its allocation), so
  overlapped writes are overwrites inside a sized file rather than
  serialized extending writes; writer reaps non-blocking-first and
  feeds its queue before waiting; stage queues deepened 2 → 8.
- **Byte-identity**: A/B vs the 0.3.0 binary at 37 MiB — all six
  profiles plus needle+null, threads 1/4, LF, custom line-len:
  identical files and indexes, 10/10.
- **Gate**: 22/22 tests, clippy clean host + windows-gnu, fmt clean.
- **Field run (2026-11-01): diagnosis DISPROVED.** 0:36 at
  287.6 MB/s — statistically the same as 0.3.1's 299.1, still ~2.2×
  below 0.3.0's 640.1, with SHA `b6a6ca0a…` identical again.
  Pre-sizing the EOF changed nothing, so extending writes were not
  the cause: the overlapped submission path itself is the cost on
  this machine (NTFS serializes concurrent non-cached writes to one
  file internally, so the queue depth never materializes, while
  each overlapped op pays full async-IRP + completion latency the
  synchronous path avoids). 0.3.3 reverts to 0.3.0's proven
  synchronous writer, keeping the neutral wins (SWAR counting,
  thread cap 16, deeper queues).

# Quality — TextSwell 0.3.1

## 0.3.1 write-path slice — verified (VM), field run owed

- **What changed**: Windows output handle is now overlapped; the
  writer keeps up to 4 writes in flight and recycles a buffer only
  after its write is reaped (error paths drain all outstanding
  writes first). Sequencer newline counting is SWAR (word-at-a-time).
  Auto thread cap raised 8 → 16.
- **Byte-identity vs 0.3.0 (the point of the slice)**: direct A/B on
  this VM at 37 MiB — all six profiles plus needle+null-rate,
  threads 1 and 4, LF eol, and custom line-len cases: file SHA-256
  and `.index` contents identical in all 10 cases.
- **Gate**: 22/22 tests (incl. new SWAR adversarial-count test),
  clippy clean on host and windows-gnu targets, fmt clean.
- **Field run (2026-11-01): correctness pass, speed REGRESSION.**
  Output SHA-256 `b6a6ca0a…` — identical to the 0.3.0 run, exact
  size, 21 needles: the byte-identity promise held in the field.
  But the run took 0:34 at 299.1 MB/s against 0.3.0's 0:16 /
  640.1 MB/s. Diagnosis: the file's end-of-file was still 0 at
  create (only allocation was pre-set), so every overlapped write
  was an *extending* write; NTFS serializes those to maintain the
  valid-data length across concurrent writes, collapsing the queue
  depth the slice was built to create (plus the writer reaped
  blocking-style, starving its own queue). 0.3.2 pre-sizes the EOF
  at create and makes reaping non-blocking-first.

# Quality — TextSwell 0.3.0

## 0.3.0 parallel generation — verified (VM), field run owed

- **What changed**: generation split across worker threads; every
  block generated independently from `range_seed(seed, block index)`;
  a sequencer restores order and applies null forcing / needle
  stamping / line counting; claim-ordered block pool (a worker takes
  its buffer only when its block is the designated next recipient —
  the earliest unfinished block can never be starved, so the pool
  cannot deadlock); `--threads <N>` (0 = auto: cores − 2, max 8).
- **Gate**: 21/21 tests (incl. new `output_does_not_depend_on_thread_count`
  and a multi-block, multi-worker end-to-end comparison against a
  sequential reference stream, needle index rows checked), clippy
  clean, fmt clean.
- **Design failure found and fixed during this slice**: the first
  draft handed pool buffers out unordered; at 1 GiB the pool could be
  hoarded by later blocks while the earliest block's worker waited —
  a hard stall, reproduced on this VM. Replaced by per-block claims +
  claim-ordered handout; the same 1 GiB runs then complete.
- **Benchmarks (honest limits)**: a same-machine A/B against a rebuilt
  0.2.0 binary (interleaved, median of 3, 1 GiB) showed 1.00–1.05× —
  this VM's storage is the binding constraint for both versions
  (dirty-hex identical at 25 MiB/s; system iowait ~73% during runs),
  so the VM cannot measure the generation gain. The gain targets
  machines where generation was the ceiling: on the owner's Windows
  machine the 0.2.0 field run averaged 207.5 MB/s while the write
  path peaked at 462.5 MB/s. **Field verification of 0.3.0 on that
  machine is the deciding measurement and is still owed.**
- **Memory**: resident set ~19 MiB observed during a 1 GiB run with
  2 generator threads (pool of 8 × 2 MiB + stages) — above 0.2.0's
  11.2 MiB because the pool now scales with thread count (max 16
  blocks = 32 MiB of buffers). Still constant in the file size.
- **Stream change**: per-block seeding means 0.3.0 bytes differ from
  0.2.0 for every profile at the same seed/settings (documented in
  the CHANGELOG); determinism within 0.3.0 is test-pinned.
- **Field run on the owner's Windows 11 machine (2026-11-01)**: the
  0.3.0 exe repeated the 10 GiB needle-bench run in 0:16 at an average
  of 640.1 MB/s with 8 generator threads (auto) — 3.1× the 0.2.0
  field run (0:49, 207.5 MB/s), exact size, 21 needle placements +
  index, stream SHA-256 `b6a6ca0a…`. Parallel generation is therefore
  field-verified as the right lever; the remaining known ceilings are
  the synchronous single-write path (queue depth 1) and the
  sequencer's per-block newline scan, both addressed in 0.3.1.

# Quality — TextSwell 0.2.0

## 0.2.0 speed slice — verified

- **Tests: 20/20 pass**, including two new pins: the SHA-NI engine
  produces digests identical to the software engine, and the threaded
  `run()` output is byte-identical to a sequential reference stream
  (with the needle index contents checked row by row).
- **Chunk-independence regression test**: `dirty-hex` and `infinite-line`
  produce identical streams regardless of fill-request slicing. This test
  was written after an A/B comparison caught `dirty-hex` changing with
  the pipeline's block size; the profile now generates in fixed 64 KiB
  segments and `infinite-line` serves from a one-word reservoir.
- **Cross-version identity (50 MiB per profile, same seed)**: 0.2.0
  output is byte-identical to 0.1.0 for dense-code, infinite-line,
  ragged-csv, unicode-stress, and needle-bench (including Sean's
  field-run settings for needle-bench). `dirty-hex` differs by design —
  see CHANGELOG 0.2.0.
- **Throughput on this VM (1 GiB per profile, into the page cache — a
  CPU-path measurement, not a disk measurement)**: dense-code
  222 → 709 MiB/s, needle-bench 286 → 985 MiB/s, infinite-line
  268 → 831 MiB/s, dirty-hex 294 → 1040 MiB/s, unicode-stress
  234 → 698 MiB/s, ragged-csv 238 → 598 MiB/s (2.5–3.5× over 0.1.0).
- **Peak RSS 11.2 MiB** while generating 1 GiB (pool of 4 × 2 MiB).
- Windows-gnu exe rebuilt from the final tree (456,192 bytes).
- **Field run on the owner's Windows 11 machine (2026-11-01)**: the
  released 0.2.0 exe repeated the 10 GiB needle-bench run in 0:49 at an
  average of 207.5 MB/s (peak snapshot 462.5 MB/s), against the 0.1.0
  runs' 113.6–132.2 MB/s on the same machine — a 1.6–1.8× field gain,
  with the SHA-256 identical again (`7bd8222a…`), exact size, and 21
  needle placements + index. The remaining gap to the VM figures is the
  real unbuffered drive write plus single-threaded generation sharing
  the ceiling.

---

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

## v0.2.0 release packaging (2026-11-01)

Full V2.0 matrix built locally from the published tree: NSIS
`TextSwell-v0.2.0-x64-Setup.exe` (302,359 B), wixl-built
`TextSwell-v0.2.0-x64-Setup.msi` (255,488 B), `TextSwell-v0.2.0-x64-Portable.zip`
(235,220 B), plus `SHA256SUMS.txt`. The MSI was verified by extraction: it
installs the byte-identical release exe (SHA-256 `0f5c626c…`) with README
and LICENSE under Program Files\TextSwell, ProductVersion 0.2.0. The
installers themselves have not yet been run on Windows — that check is
owed on the owner's machine. Installer sources live in `installer/`.

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
