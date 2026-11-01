# Parity — TextSwell

TextSwell has exactly one surface: the `largetxt-gen` command line. There
is no GUI, config file, or library API, so parity means every documented
capability is reachable from the CLI and behaves the same on the Windows
writer and the non-Windows fallback writer (output bytes and sizes
identical; only write behaviour differs).

| Capability | CLI entry | Status |
|---|---|---|
| Generate a file | `<OUTPUT_PATH> --size` | Done |
| Choose content shape | `--profile` (6 profiles) | Done |
| Line length control | `--line-len MIN:MAX` | Done |
| Line endings | `--eol crlf\|lf\|cr\|mixed` | Done |
| Needle injection + index | `--needle`, `--needle-every`, `<output>.index` | Done |
| NUL forcing | `--null-rate` | Done |
| Unbuffered toggle | `--unbuffered <bool>` | Done |
| Repeatable output | `--seed <HEX>` | Done |
| Progress + ETA | stderr, every 100 ms | Done |
| Checksum of written stream | SHA-256 in the summary | Done |
| Help / version | `--help`, `--version` | Done |

Anything not listed here does not exist in the tool; there are no hidden
modes.
