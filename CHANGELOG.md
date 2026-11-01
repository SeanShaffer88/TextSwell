# Changelog

## 0.1.0 — 2026-11-01

Initial release.

- `largetxt-gen` CLI with an in-house, zero-dependency argument parser: output path, `--size` (K/M/G/T, fractions, plain bytes), `--profile`, `--line-len MIN:MAX`, `--eol crlf|lf|cr|mixed`, `--needle`, `--needle-every`, `--null-rate`, `--unbuffered`, `--seed` (hex), `--help`, `--version`. Exit codes 0/1/2.
- Six generation profiles: `dense-code`, `infinite-line`, `ragged-csv`, `dirty-hex`, `unicode-stress`, `needle-bench`.
- Needle stamping at exact byte intervals on any profile, with an `<output>.index` sidecar listing byte offset and 1-based line number per needle.
- Windows I/O: `CreateFileW` with `FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN | FILE_SHARE_READ`, `FILE_ALLOCATION_INFO` pre-allocation via `SetFileInformationByHandle`, 4 MiB `VirtualAlloc` sector-aligned write block, sector-padded final write cut back to the exact size with `SetFilePointerEx` + `SetEndOfFile`.
- Deterministic Xoshiro256++ generation from a 64-bit seed; built-in streaming SHA-256 printed in the completion summary.
- `QueryPerformanceCounter` progress reporting every 100 ms: progress bar, percent, instant and average throughput, elapsed time, ETA.
- Constant memory footprint regardless of output size.
