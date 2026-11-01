# Changelog

## 0.3.3 — 2026-11-01

Revert of the overlapped writer. **Output remains byte-identical to
0.3.0 for every profile and setting** (verified by direct
comparison, including needle indexes).

- **Synchronous writer restored**: the 0.3.1/0.3.2 field runs
  measured 299 and 288 MB/s against 0.3.0's 640 MB/s with identical
  output, and 0.3.2's pre-sized-EOF fix changed nothing — the
  overlapped path itself was the cost (NTFS serializes concurrent
  non-cached writes to one file internally, so the queue depth
  never materialized while every operation paid asynchronous
  latency). The writer is back to 0.3.0's synchronous `WriteFile`
  loop; the `FILE_FLAG_OVERLAPPED` machinery and the EOF pre-sizing
  are removed.
- Kept from the 0.3.1/0.3.2 slices, being orthogonal to the
  regression: SWAR newline counting, the `--threads` auto cap of
  16, and the deeper stage queues.

## 0.3.2 — 2026-11-01

Fix for the 0.3.1 field regression. **Output remains byte-identical
to 0.3.0/0.3.1 for every profile and setting** (verified by direct
comparison, including needle indexes).

- **Pre-sized end-of-file on Windows**: 0.3.1's field run was slower
  than 0.3.0 (299 vs 640 MB/s) despite correct output. Cause: the
  file's end-of-file was still 0 when writing began (only its
  allocation was pre-set), so every overlapped write extended the
  file — and NTFS serializes extending writes to maintain the
  valid-data length, collapsing the write queue to depth one. The
  file is now sized to its final padded length at creation, so
  writes are plain overwrites that complete concurrently; the run
  still truncates to the exact requested size at the end. (Side
  effect: the file shows its full size from the start.)
- **Non-blocking reap first**: the writer now checks for completed
  writes without waiting, and only blocks on a reap when the stream
  has ended or the queue is full — previously it could wait on the
  drive while finished blocks sat unsubmitted.
- Deeper sequencer/hasher queues (2 → 8 blocks of slack).

## 0.3.1 — 2026-11-01

Write-path slice: same bytes, deeper write queue. **Output is
byte-identical to 0.3.0 for every profile and setting** (verified by
direct comparison, including needle indexes).

- **Overlapped writes on Windows**: the output file is opened with
  `FILE_FLAG_OVERLAPPED` and the writer keeps up to four writes in
  flight instead of waiting for each 2 MiB write to finish before
  starting the next — NVMe drives need queue depth to reach full
  sequential bandwidth. A buffer returns to the pool only after its
  write has been reaped, and shutdown paths drain every outstanding
  write before anything is freed.
- **Faster line counting**: the sequencer's per-block newline count
  (which feeds needle line numbers) now uses a SWAR word-at-a-time
  scan instead of a byte loop, with an adversarial-content test
  pinning it against the naive count.
- **Higher auto thread cap**: `--threads` auto now allows up to 16
  generator threads (cores − 2), for many-core machines.

## 0.3.0 — 2026-11-01

Parallel generation: more generator threads on the same pipeline.

- **Multi-threaded generation**: generation is split across worker
  threads (default: auto — two fewer than the core count, up to 8;
  override with `--threads <N>`). Every 2 MiB block is generated
  independently from a seed derived from (seed, block index), so the
  output depends only on the command, never on the machine or the
  thread count — a test pins byte-identity between 1 and 4 workers.
  A sequencer stage restores block order and applies the
  order-sensitive passes (null forcing, needle stamping, line
  counting) before hashing and writing, exactly as before.
- **Output stream change (all profiles)**: because content is now
  seeded per block rather than drawn from one continuous stream, the
  same seed and settings produce different bytes in 0.3.0 than in
  0.2.0. Within 0.3.0 the output is fully deterministic, and needle
  indexes remain exact (`byte_offset` / `line_number` verified
  end-to-end by the test suite).
- **Deadlock-free block pool**: pool buffers are handed out in block
  order, which guarantees the earliest unfinished block can never be
  starved of a buffer by later blocks finishing early. (An earlier
  draft of this slice used unordered handout and could stall at
  large sizes; the shipped design removes that failure mode by
  construction, and the gate covers multi-block, multi-worker runs.)
- Memory remains bounded and size-independent: the pool grows with
  the thread count (threads + 6 blocks, max 16); measured resident
  set about 19 MiB with two generator threads while writing 1 GiB.

## 0.2.0 — 2026-11-01

Speed slice: the same bytes, produced much faster.

- **Three-stage pipeline**: generation, SHA-256, and writing now run on
  their own threads over a pool of four 2 MiB sector-aligned blocks
  (previously one 4 MiB block, strictly serial). Blocks stay in order
  through every queue, so output is byte-identical to 0.1.0 for
  `dense-code`, `infinite-line`, `ragged-csv`, `unicode-stress`, and
  `needle-bench` (verified by direct comparison at 50 MiB per profile).
  Measured on the reference machine: 2.5–3.5× faster across profiles
  (e.g. dense-code 222 → 709 MiB/s, needle-bench 286 → 985 MiB/s, into
  the OS page cache). Peak memory is unchanged in shape: 11.2 MiB RSS
  while generating 1 GiB.
- **Hardware SHA-256**: on x86_64 CPUs with SHA extensions, compression
  uses SHA-NI instructions; the software implementation remains as the
  fallback and for all other architectures. Tests prove both engines
  produce identical digests, including the FIPS vectors.
- **`dirty-hex` stream change (the one intentional output change)**: the
  profile is now generated in fixed 64 KiB segments, and `infinite-line`
  now serves bytes from a one-word reservoir. Both profiles are now
  provably independent of the caller's fill chunking (a regression test
  pins this), so an internal block-size change can never again silently
  change what a seed produces. A given `dirty-hex` seed therefore makes
  different bytes in 0.2.0 than in 0.1.0; every other profile is
  byte-identical across the two versions.

## 0.1.0 — 2026-11-01

Initial release.

- `largetxt-gen` CLI with an in-house, zero-dependency argument parser: output path, `--size` (K/M/G/T, fractions, plain bytes), `--profile`, `--line-len MIN:MAX`, `--eol crlf|lf|cr|mixed`, `--needle`, `--needle-every`, `--null-rate`, `--unbuffered`, `--seed` (hex), `--help`, `--version`. Exit codes 0/1/2.
- Six generation profiles: `dense-code`, `infinite-line`, `ragged-csv`, `dirty-hex`, `unicode-stress`, `needle-bench`.
- Needle stamping at exact byte intervals on any profile, with an `<output>.index` sidecar listing byte offset and 1-based line number per needle.
- Windows I/O: `CreateFileW` with `FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN | FILE_SHARE_READ`, `FILE_ALLOCATION_INFO` pre-allocation via `SetFileInformationByHandle`, 4 MiB `VirtualAlloc` sector-aligned write block, sector-padded final write cut back to the exact size with `SetFilePointerEx` + `SetEndOfFile`.
- Deterministic Xoshiro256++ generation from a 64-bit seed; built-in streaming SHA-256 printed in the completion summary.
- `QueryPerformanceCounter` progress reporting every 100 ms: progress bar, percent, instant and average throughput, elapsed time, ETA.
- Constant memory footprint regardless of output size.
