//! # TextSwell — `largetxt-gen`
//!
//! A zero-dependency Windows command-line utility whose sole purpose is to
//! saturate disk I/O while generating massive test files (multi-gigabyte to
//! multi-terabyte) engineered to stress a high-performance Windows text/hex
//! editor: its memory-mapped sliding windows, 64-bit piece tables,
//! DirectWrite layouts, and search routines.
//!
//! ## Design guarantees
//!
//! * **Zero external crates.** Only `std` plus raw `extern "system"`
//!   declarations against `kernel32.dll`. The SHA-256 implementation and the
//!   Xoshiro256++ PRNG are written out in this file.
//! * **Constant memory.** One sector-aligned 4 MiB write block (allocated
//!   once with `VirtualAlloc`), one 16 KiB line-staging buffer, and small
//!   fixed state. Resident set size stays under 16 MB whether the output is
//!   500 MB or 100 GB.
//! * **Unbuffered, pre-allocated writes on Windows.** `CreateFileW` with
//!   `FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN`, NTFS
//!   pre-allocation through `SetFileInformationByHandle`
//!   (`FILE_ALLOCATION_INFO`), and a final `SetFilePointerEx` +
//!   `SetEndOfFile` truncation so the file ends at the *exact* requested
//!   byte even though unbuffered writes must be sector-sized.
//! * **Deterministic.** The same `--seed`, size, profile, and flags always
//!   produce byte-identical output and therefore the same SHA-256.
//!
//! On non-Windows hosts the Win32 layer is replaced by a thin `std::fs`
//! fallback with the same semantics (aligned buffer, pre-allocation via
//! `set_len`, exact final size). That fallback exists so the generator,
//! parser, checksum, and profiles can be built and tested anywhere; the
//! shipped Windows binary always takes the Win32 path.
//!
//! ## Exit codes
//!
//! * `0` — success.
//! * `1` — command-line / usage error.
//! * `2` — I/O error (create, write, truncate, or sidecar failure).

#![forbid(unsafe_op_in_unsafe_fn)]
// The explicit `return`s in the cfg(windows) / cfg(not(windows)) blocks of
// the I/O layer are load-bearing for readability of the two platform paths.
#![allow(clippy::needless_return)]

use std::io::Write as _;
use std::time::Instant;

// ===========================================================================
// Section 1 — Raw Win32 bindings (Windows only)
// ===========================================================================
//
// Only the handful of kernel32 entry points this tool needs are declared.
// Nothing here depends on the `windows` or `windows-sys` crates.

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;
    pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const FILE_SHARE_READ: u32 = 0x0000_0001;
    pub const CREATE_ALWAYS: u32 = 2;
    pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    pub const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    pub const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;

    pub const MEM_COMMIT: u32 = 0x0000_1000;
    pub const MEM_RESERVE: u32 = 0x0000_2000;
    pub const MEM_RELEASE: u32 = 0x0000_8000;
    pub const PAGE_READWRITE: u32 = 0x04;

    pub const FILE_BEGIN: u32 = 0;
    /// `FILE_INFO_BY_HANDLE_CLASS::FileAllocationInfo`.
    pub const FILE_ALLOCATION_INFO_CLASS: i32 = 5;

    /// Mirrors `FILE_ALLOCATION_INFO` from the Windows SDK.
    #[repr(C)]
    pub struct FileAllocationInfo {
        pub allocation_size: i64,
    }

    extern "system" {
        pub fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        pub fn WriteFile(
            file: Handle,
            buffer: *const u8,
            number_of_bytes_to_write: u32,
            number_of_bytes_written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        pub fn CloseHandle(object: Handle) -> i32;
        pub fn FlushFileBuffers(file: Handle) -> i32;
        pub fn SetFilePointerEx(
            file: Handle,
            distance_to_move: i64,
            new_file_pointer: *mut i64,
            move_method: u32,
        ) -> i32;
        pub fn SetEndOfFile(file: Handle) -> i32;
        pub fn SetFileInformationByHandle(
            file: Handle,
            file_information_class: i32,
            file_information: *mut c_void,
            buffer_size: u32,
        ) -> i32;
        pub fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        pub fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
        pub fn QueryPerformanceCounter(performance_count: *mut i64) -> i32;
        pub fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
        pub fn GetLastError() -> u32;
    }

    /// Last Win32 error code, formatted for an error message.
    pub fn last_error() -> u32 {
        unsafe { GetLastError() }
    }
}

/// Sector size assumed for unbuffered alignment. 4 KiB is the sector size of
/// every modern NVMe/SSD volume and a multiple of the older 512-byte sector,
/// so aligning writes to 4 KiB satisfies `FILE_FLAG_NO_BUFFERING` on both.
const SECTOR_SIZE: u64 = 4096;

/// Size of the single reusable write block: 4 MiB, a multiple of the sector
/// size and large enough to keep an NVMe queue saturated with plain
/// synchronous `WriteFile` calls.
const BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// Rounds `n` up to the next multiple of `align` (a power of two).
fn round_up(n: u64, align: u64) -> u64 {
    (n + align - 1) & !(align - 1)
}

// ===========================================================================
// Section 2 — High-resolution timer (QueryPerformanceCounter on Windows)
// ===========================================================================

struct PerfTimer {
    #[cfg_attr(not(windows), allow(dead_code))]
    start_counter: i64,
    #[cfg_attr(not(windows), allow(dead_code))]
    frequency: i64,
    fallback: Instant,
}

impl PerfTimer {
    fn new() -> Self {
        #[cfg(windows)]
        unsafe {
            let mut freq: i64 = 0;
            let mut counter: i64 = 0;
            if win::QueryPerformanceFrequency(&mut freq) != 0
                && freq > 0
                && win::QueryPerformanceCounter(&mut counter) != 0
            {
                return PerfTimer {
                    start_counter: counter,
                    frequency: freq,
                    fallback: Instant::now(),
                };
            }
        }
        PerfTimer {
            start_counter: 0,
            frequency: 0,
            fallback: Instant::now(),
        }
    }

    /// Elapsed wall-clock seconds since this timer was created.
    fn elapsed_secs(&self) -> f64 {
        #[cfg(windows)]
        if self.frequency > 0 {
            let mut counter: i64 = 0;
            if unsafe { win::QueryPerformanceCounter(&mut counter) } != 0 {
                return (counter - self.start_counter) as f64 / self.frequency as f64;
            }
        }
        self.fallback.elapsed().as_secs_f64()
    }
}

// ===========================================================================
// Section 3 — Xoshiro256++ PRNG (deterministic, zero-heap, inlined)
// ===========================================================================
//
// Reference algorithm by Blackman & Vigna. Period 2^256 - 1, excellent
// statistical quality, and only a few ALU ops per 64-bit word, which is what
// keeps generation from becoming the bottleneck in front of the disk.

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[derive(Clone)]
struct Xoshiro256PlusPlus {
    s: [u64; 4],
}

impl Xoshiro256PlusPlus {
    /// Expands a single 64-bit seed into the full 256-bit state with
    /// SplitMix64. A zero seed is nudged so the state can never be all-zero
    /// (the one state Xoshiro must never sit in).
    fn from_seed(seed: u64) -> Self {
        let mut sm = SplitMix64 {
            state: seed ^ 0xA076_1D64_78BD_642F,
        };
        let mut s = [sm.next_u64(), sm.next_u64(), sm.next_u64(), sm.next_u64()];
        if s.iter().all(|&x| x == 0) {
            s[0] = 0x9E37_79B9_7F4A_7C15;
        }
        Xoshiro256PlusPlus { s }
    }

    #[inline(always)]
    fn next_u64(&mut self) -> u64 {
        let result = (self.s[0].wrapping_add(self.s[3]))
            .rotate_left(23)
            .wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// 32-bit draw, used by short-field generation paths and tests.
    #[inline(always)]
    #[allow(dead_code)]
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform-ish value in `0..n`. `n` must be non-zero. Modulo bias is
    /// irrelevant for test-data generation.
    #[inline(always)]
    fn below(&mut self, n: u64) -> u64 {
        debug_assert!(n > 0);
        self.next_u64() % n
    }

    /// Value in the inclusive range `lo..=hi`.
    #[inline(always)]
    fn range_inclusive(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            lo
        } else {
            lo + self.below(hi - lo + 1)
        }
    }

    /// True with probability `pct / 100` (pct may be fractional down to
    /// 0.01 resolution).
    #[inline(always)]
    fn chance_pct(&mut self, pct: f64) -> bool {
        if pct <= 0.0 {
            return false;
        }
        if pct >= 100.0 {
            return true;
        }
        (self.below(10_000) as f64) < pct * 100.0
    }

    /// Fills `out` with raw PRNG bytes, 8 at a time.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    #[inline]
    fn fill_bytes(&mut self, out: &mut [u8]) {
        let mut chunks = out.chunks_exact_mut(8);
        for c in &mut chunks {
            c.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        let rem = chunks.into_remainder();
        if !rem.is_empty() {
            let bytes = self.next_u64().to_le_bytes();
            rem.copy_from_slice(&bytes[..rem.len()]);
        }
    }

    /// Picks one element of a non-empty slice.
    #[inline]
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

// ===========================================================================
// Section 4 — SHA-256 (FIPS 180-4, written out; no crate)
// ===========================================================================
//
// Streaming hasher: feed each generated block before it is written, then
// read the hex digest at the end. The digest covers exactly the logical
// bytes of the file — never the sector padding used for unbuffered writes.

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    fn compress(state: &mut [u32; 8], block: &[u8]) {
        debug_assert_eq!(block.len(), 64);
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        );
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buffer_len > 0 {
            let take = std::cmp::min(64 - self.buffer_len, data.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                Self::compress(&mut self.state, &block);
                self.buffer_len = 0;
            }
        }
        while data.len() >= 64 {
            Self::compress(&mut self.state, &data[..64]);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    fn finalize_hex(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        // Padding: 0x80, zeros, then the 64-bit big-endian bit length.
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        let pad_len = if self.buffer_len < 56 {
            56 - self.buffer_len
        } else {
            120 - self.buffer_len
        };
        // Feed padding through update() without corrupting total_len: save it.
        let saved_total = self.total_len;
        self.update(&pad[..pad_len]);
        self.update(&bit_len.to_be_bytes());
        self.total_len = saved_total;
        debug_assert_eq!(self.buffer_len, 0);
        let mut out = String::with_capacity(64);
        for word in self.state.iter() {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }
}

// ===========================================================================
// Section 5 — Command-line model and the in-house argument parser
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Profile {
    DenseCode,
    InfiniteLine,
    RaggedCsv,
    DirtyHex,
    UnicodeStress,
    NeedleBench,
}

impl Profile {
    fn from_name(name: &str) -> Option<Profile> {
        match name {
            "dense-code" => Some(Profile::DenseCode),
            "infinite-line" => Some(Profile::InfiniteLine),
            "ragged-csv" => Some(Profile::RaggedCsv),
            "dirty-hex" => Some(Profile::DirtyHex),
            "unicode-stress" => Some(Profile::UnicodeStress),
            "needle-bench" => Some(Profile::NeedleBench),
            _ => None,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Profile::DenseCode => "dense-code",
            Profile::InfiniteLine => "infinite-line",
            Profile::RaggedCsv => "ragged-csv",
            Profile::DirtyHex => "dirty-hex",
            Profile::UnicodeStress => "unicode-stress",
            Profile::NeedleBench => "needle-bench",
        }
    }
    /// Default line-length range for the line-oriented profiles.
    fn default_line_range(&self) -> (usize, usize) {
        match self {
            Profile::DenseCode => (40, 120),
            Profile::RaggedCsv => (10, 8000),
            Profile::UnicodeStress => (20, 200),
            Profile::NeedleBench => (60, 110),
            Profile::InfiniteLine | Profile::DirtyHex => (0, 0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Eol {
    Crlf,
    Lf,
    Cr,
    Mixed,
}

impl Eol {
    fn from_name(name: &str) -> Option<Eol> {
        match name {
            "crlf" => Some(Eol::Crlf),
            "lf" => Some(Eol::Lf),
            "cr" => Some(Eol::Cr),
            "mixed" => Some(Eol::Mixed),
            _ => None,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Eol::Crlf => "crlf",
            Eol::Lf => "lf",
            Eol::Cr => "cr",
            Eol::Mixed => "mixed",
        }
    }
    /// Writes the terminator chosen for one line into `out` (capacity >= 2)
    /// and returns its length. `Mixed` picks per line from the PRNG.
    fn write_into(&self, rng: &mut Xoshiro256PlusPlus, out: &mut [u8]) -> usize {
        match self {
            Eol::Crlf => {
                out[0] = b'\r';
                out[1] = b'\n';
                2
            }
            Eol::Lf => {
                out[0] = b'\n';
                1
            }
            Eol::Cr => {
                out[0] = b'\r';
                1
            }
            Eol::Mixed => match rng.below(4) {
                0 | 1 => {
                    out[0] = b'\r';
                    out[1] = b'\n';
                    2
                }
                2 => {
                    out[0] = b'\n';
                    1
                }
                _ => {
                    out[0] = b'\r';
                    1
                }
            },
        }
    }
}

struct Config {
    output_path: String,
    size: u64,
    profile: Profile,
    line_min: usize,
    line_max: usize,
    line_len_custom: bool,
    eol: Eol,
    needle: Option<Vec<u8>>,
    needle_every: u64,
    null_rate: f64,
    unbuffered: bool,
    seed: u64,
}

const DEFAULT_SIZE: u64 = 1024 * 1024 * 1024; // 1 GiB
const DEFAULT_SEED: u64 = 0x5EED_5EED_5EED_5EED;
/// Spec default for needle-bench: a needle every 500 MiB.
const DEFAULT_NEEDLE_EVERY: u64 = 500 * 1024 * 1024;

/// Parses a human size such as `512M`, `10G`, `1T`, `4096`, `1.5G`, `64KB`.
/// Suffixes are binary (1K = 1024). Returns `None` on malformed input or
/// overflow past `u64::MAX`.
fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let upper = t.to_ascii_uppercase();
    let (num_part, mult): (&str, u64) = if let Some(p) = upper.strip_suffix("KB") {
        (p, 1024)
    } else if let Some(p) = upper.strip_suffix("MB") {
        (p, 1024 * 1024)
    } else if let Some(p) = upper.strip_suffix("GB") {
        (p, 1024 * 1024 * 1024)
    } else if let Some(p) = upper.strip_suffix("TB") {
        (p, 1024 * 1024 * 1024 * 1024)
    } else if let Some(p) = upper.strip_suffix('K') {
        (p, 1024)
    } else if let Some(p) = upper.strip_suffix('M') {
        (p, 1024 * 1024)
    } else if let Some(p) = upper.strip_suffix('G') {
        (p, 1024 * 1024 * 1024)
    } else if let Some(p) = upper.strip_suffix('T') {
        (p, 1024 * 1024 * 1024 * 1024)
    } else if let Some(p) = upper.strip_suffix('B') {
        (p, 1)
    } else {
        (upper.as_str(), 1)
    };
    let num_part = num_part.trim();
    if num_part.is_empty() || !num_part.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if let Ok(v) = num_part.parse::<u64>() {
        return v.checked_mul(mult);
    }
    let f: f64 = num_part.parse().ok()?;
    if !f.is_finite() || f < 0.0 {
        return None;
    }
    let bytes = f * mult as f64;
    if bytes > u64::MAX as f64 {
        return None;
    }
    Some(bytes as u64)
}

/// Parses `--seed`: hex, with or without a `0x` prefix. A bare run of digits
/// is still read as hex, per the flag's contract (`--seed <HEX>`); pass a
/// `0x` prefix to remove all doubt.
fn parse_seed(text: &str) -> Option<u64> {
    let t = text.trim();
    let hex = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    if hex.is_empty() || hex.len() > 16 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// Parses `--line-len MIN:MAX`.
fn parse_line_len(text: &str) -> Option<(usize, usize)> {
    let (a, b) = text.split_once(':')?;
    let min: usize = a.trim().parse().ok()?;
    let max: usize = b.trim().parse().ok()?;
    if min == 0 || max < min {
        return None;
    }
    Some((min, max))
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn print_help() {
    let text = "\
TextSwell - largetxt-gen 0.1.0
Generate massive test files that stress large-file text/hex editors.

USAGE:
    largetxt-gen <OUTPUT_PATH> [OPTIONS]

ARGS:
    <OUTPUT_PATH>            Target output file (created or overwritten)

OPTIONS:
    -s, --size <SIZE>        Total size: 512M, 10G, 1T, 1.5G, 4096 (default: 1G)
    -p, --profile <PROFILE>  dense-code | infinite-line | ragged-csv |
                             dirty-hex | unicode-stress | needle-bench
                             (default: dense-code)
        --line-len <MIN:MAX> Line length range for line profiles, e.g. 80:120
        --eol <MODE>         crlf | lf | cr | mixed (default: crlf;
                             ragged-csv defaults to mixed unless set)
        --needle <TEXT>      Needle string injected for search tests
                             (needle-bench default: TARGET_STRING)
        --needle-every <SIZE> Spacing between needles (default: 500M)
        --null-rate <PCT>    Percent of bytes forced to 0x00, 0.0-100.0
        --unbuffered <BOOL>  Use FILE_FLAG_NO_BUFFERING on Windows
                             (default: true)
        --seed <HEX>         64-bit hex seed for repeatable output
    -h, --help               Show this help
    -V, --version            Show version

NOTES:
    With needle injection active, an <output>.index sidecar is written with
    one 'byte_offset<TAB>line_number<TAB>needle' row per needle.
    Exit codes: 0 success, 1 usage error, 2 I/O error.
";
    print!("{text}");
}

/// Parses `std::env::args`. On error returns a message for the caller to
/// print (exit code 1). `--help` / `--version` are reported as errors whose
/// text the caller prints to stdout with exit code 0 — see `main`.
fn parse_args() -> Result<Config, (String, i32)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut output_path: Option<String> = None;
    let mut size: u64 = DEFAULT_SIZE;
    let mut profile = Profile::DenseCode;
    let mut line_len: Option<(usize, usize)> = None;
    let mut eol: Option<Eol> = None;
    let mut needle: Option<Vec<u8>> = None;
    let mut needle_every: u64 = DEFAULT_NEEDLE_EVERY;
    let mut null_rate: f64 = 0.0;
    let mut unbuffered = true;
    let mut seed: u64 = DEFAULT_SEED;

    let usage_err = |msg: &str| -> (String, i32) {
        (
            format!("error: {msg}\nRun 'largetxt-gen --help' for usage.\n"),
            1,
        )
    };

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].clone();
        // Split `--flag=value` into flag + inline value.
        let (flag, inline_val): (String, Option<String>) = if arg.starts_with("--") {
            match arg.split_once('=') {
                Some((f, v)) => (f.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            }
        } else {
            (arg.clone(), None)
        };
        let take_value = |i: &mut usize, name: &str| -> Result<String, (String, i32)> {
            if let Some(v) = &inline_val {
                return Ok(v.clone());
            }
            *i += 1;
            if *i >= args.len() {
                return Err(usage_err(&format!("{name} needs a value")));
            }
            Ok(args[*i].clone())
        };

        match flag.as_str() {
            "-h" | "--help" => {
                print_help();
                return Err((String::new(), 0));
            }
            "-V" | "--version" => {
                return Err(("largetxt-gen 0.1.0\n".to_string(), 0));
            }
            "-s" | "--size" => {
                let v = take_value(&mut i, "--size")?;
                size =
                    parse_size(&v).ok_or_else(|| usage_err(&format!("bad --size value '{v}'")))?;
            }
            "-p" | "--profile" => {
                let v = take_value(&mut i, "--profile")?;
                profile = Profile::from_name(&v)
                    .ok_or_else(|| usage_err(&format!("unknown --profile '{v}'")))?;
            }
            "--line-len" => {
                let v = take_value(&mut i, "--line-len")?;
                line_len =
                    Some(parse_line_len(&v).ok_or_else(|| {
                        usage_err(&format!("bad --line-len '{v}' (want MIN:MAX)"))
                    })?);
            }
            "--eol" => {
                let v = take_value(&mut i, "--eol")?;
                eol = Some(
                    Eol::from_name(&v.to_ascii_lowercase())
                        .ok_or_else(|| usage_err(&format!("bad --eol '{v}'")))?,
                );
            }
            "--needle" => {
                let v = take_value(&mut i, "--needle")?;
                if v.is_empty() {
                    return Err(usage_err("--needle must not be empty"));
                }
                needle = Some(v.into_bytes());
            }
            "--needle-every" => {
                let v = take_value(&mut i, "--needle-every")?;
                needle_every = parse_size(&v)
                    .ok_or_else(|| usage_err(&format!("bad --needle-every '{v}'")))?;
                if needle_every == 0 {
                    return Err(usage_err("--needle-every must be larger than 0"));
                }
            }
            "--null-rate" => {
                let v = take_value(&mut i, "--null-rate")?;
                let parsed: f64 = v
                    .parse()
                    .map_err(|_| usage_err(&format!("bad --null-rate '{v}'")))?;
                if !(0.0..=100.0).contains(&parsed) {
                    return Err(usage_err("--null-rate must be between 0.0 and 100.0"));
                }
                null_rate = parsed;
            }
            "--unbuffered" => {
                // Accepts `--unbuffered`, `--unbuffered=true`,
                // and `--unbuffered true`.
                if let Some(v) = &inline_val {
                    unbuffered = parse_bool(v)
                        .ok_or_else(|| usage_err(&format!("bad --unbuffered value '{v}'")))?;
                } else if i + 1 < args.len() && parse_bool(&args[i + 1]).is_some() {
                    i += 1;
                    unbuffered = parse_bool(&args[i]).unwrap();
                } else {
                    unbuffered = true;
                }
            }
            "--seed" => {
                let v = take_value(&mut i, "--seed")?;
                seed = parse_seed(&v)
                    .ok_or_else(|| usage_err(&format!("bad --seed '{v}' (want 64-bit hex)")))?;
            }
            _ => {
                if flag.starts_with('-') && flag.len() > 1 {
                    return Err(usage_err(&format!("unknown option '{flag}'")));
                }
                if output_path.is_some() {
                    return Err(usage_err("only one OUTPUT_PATH may be given"));
                }
                output_path = Some(flag);
            }
        }
        i += 1;
    }

    let output_path = match output_path {
        Some(p) => p,
        None => return Err(usage_err("missing OUTPUT_PATH")),
    };

    // Profile defaults for the line range / EOL, with user values winning.
    let (def_min, def_max) = profile.default_line_range();
    let (line_min, line_max) = line_len.unwrap_or((def_min, def_max));
    let eol = eol.unwrap_or(if profile == Profile::RaggedCsv {
        Eol::Mixed
    } else {
        Eol::Crlf
    });
    // needle-bench always injects; default needle per spec.
    if profile == Profile::NeedleBench && needle.is_none() {
        needle = Some(b"TARGET_STRING".to_vec());
    }
    // A needle every N bytes must be at least as long as the needle itself,
    // otherwise injections would overlap and offsets would stop being exact.
    if let Some(n) = &needle {
        if needle_every < n.len() as u64 {
            return Err(usage_err(
                "--needle-every must be at least the needle length",
            ));
        }
    }

    Ok(Config {
        output_path,
        size,
        profile,
        line_min,
        line_max,
        line_len_custom: line_len.is_some(),
        eol,
        needle,
        needle_every,
        null_rate,
        unbuffered,
        seed,
    })
}

// ===========================================================================
// Section 6 — Sector-aligned write block (VirtualAlloc on Windows)
// ===========================================================================

/// A single 4 MiB buffer whose address is aligned to the 4 KiB page (and
/// therefore sector) boundary, as `FILE_FLAG_NO_BUFFERING` requires for the
/// buffer address, the write offset, and the write length.
struct AlignedBlock {
    ptr: *mut u8,
    len: usize,
    #[cfg(not(windows))]
    layout: std::alloc::Layout,
}

// VirtualAlloc memory and the `std::alloc` fallback are both safe to share
// here because AlignedBlock is only ever used from the single writer thread.
unsafe impl Send for AlignedBlock {}

impl AlignedBlock {
    fn new(len: usize) -> Result<Self, String> {
        #[cfg(windows)]
        unsafe {
            let ptr = win::VirtualAlloc(
                std::ptr::null_mut(),
                len,
                win::MEM_COMMIT | win::MEM_RESERVE,
                win::PAGE_READWRITE,
            );
            if ptr.is_null() {
                return Err(format!(
                    "VirtualAlloc of {len} bytes failed (Win32 error {})",
                    win::last_error()
                ));
            }
            return Ok(AlignedBlock {
                ptr: ptr as *mut u8,
                len,
            });
        }
        #[cfg(not(windows))]
        unsafe {
            let layout = std::alloc::Layout::from_size_align(len, SECTOR_SIZE as usize)
                .map_err(|e| format!("bad buffer layout: {e}"))?;
            let ptr = std::alloc::alloc(layout);
            if ptr.is_null() {
                return Err(format!("aligned allocation of {len} bytes failed"));
            }
            return Ok(AlignedBlock { ptr, len, layout });
        }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for AlignedBlock {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            win::VirtualFree(self.ptr as *mut std::ffi::c_void, 0, win::MEM_RELEASE);
        }
        #[cfg(not(windows))]
        unsafe {
            std::alloc::dealloc(self.ptr, self.layout);
        }
    }
}

// ===========================================================================
// Section 7 — Output file (Win32 unbuffered + pre-allocation; std fallback)
// ===========================================================================

struct OutputFile {
    #[cfg(windows)]
    handle: win::Handle,
    #[cfg(not(windows))]
    file: std::fs::File,
}

impl OutputFile {
    /// Creates/truncates `path`. On Windows this is `CreateFileW` with
    /// `FILE_FLAG_SEQUENTIAL_SCAN` and, when `unbuffered` is set,
    /// `FILE_FLAG_NO_BUFFERING`, sharing the file for reading so an editor
    /// can open it while it is still being written. The file is then
    /// pre-allocated to `alloc_bytes` (the padded size) with
    /// `FILE_ALLOCATION_INFO` so NTFS lays it down in one extent.
    fn create(path: &str, unbuffered: bool, alloc_bytes: u64) -> Result<Self, String> {
        #[cfg(windows)]
        unsafe {
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let mut flags = win::FILE_ATTRIBUTE_NORMAL | win::FILE_FLAG_SEQUENTIAL_SCAN;
            if unbuffered {
                flags |= win::FILE_FLAG_NO_BUFFERING;
            }
            let handle = win::CreateFileW(
                wide.as_ptr(),
                win::GENERIC_WRITE,
                win::FILE_SHARE_READ,
                std::ptr::null_mut(),
                win::CREATE_ALWAYS,
                flags,
                std::ptr::null_mut(),
            );
            if handle == win::INVALID_HANDLE_VALUE {
                return Err(format!(
                    "CreateFileW failed for '{path}' (Win32 error {})",
                    win::last_error()
                ));
            }
            let out = OutputFile { handle };
            // Zero-fragmentation pre-allocation. Failure is not fatal: some
            // filesystems (FAT, network shares) reject allocation info, and
            // the writes themselves will extend the file correctly anyway.
            if alloc_bytes > 0 {
                let mut info = win::FileAllocationInfo {
                    allocation_size: alloc_bytes as i64,
                };
                let _ = win::SetFileInformationByHandle(
                    handle,
                    win::FILE_ALLOCATION_INFO_CLASS,
                    &mut info as *mut win::FileAllocationInfo as *mut std::ffi::c_void,
                    std::mem::size_of::<win::FileAllocationInfo>() as u32,
                );
                // Allocation info can move the file pointer on some
                // filesystems; pin it back to the start.
                let mut pos: i64 = 0;
                let _ = win::SetFilePointerEx(handle, 0, &mut pos, win::FILE_BEGIN);
            }
            return Ok(out);
        }
        #[cfg(not(windows))]
        {
            use std::io::Seek as _;
            let mut file =
                std::fs::File::create(path).map_err(|e| format!("cannot create '{path}': {e}"))?;
            if alloc_bytes > 0 {
                // Pre-allocation stand-in: extending the length up front keeps
                // the observable behaviour (size, then exact truncation)
                // identical for tests on non-Windows hosts.
                let _ = file.set_len(alloc_bytes);
                let _ = file.seek(std::io::SeekFrom::Start(0));
            }
            let _ = unbuffered;
            return Ok(OutputFile { file });
        }
    }

    /// Writes every byte of `data`. With unbuffered I/O the caller
    /// guarantees `data.len()` is a sector multiple and the buffer/offset
    /// alignment holds; `WriteFile` may still write partially, so loop.
    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        #[cfg(windows)]
        unsafe {
            let mut written_total = 0usize;
            while written_total < data.len() {
                let remaining = data.len() - written_total;
                let chunk = std::cmp::min(remaining, u32::MAX as usize) as u32;
                let mut written: u32 = 0;
                let ok = win::WriteFile(
                    self.handle,
                    data.as_ptr().add(written_total),
                    chunk,
                    &mut written,
                    std::ptr::null_mut(),
                );
                if ok == 0 {
                    return Err(format!(
                        "WriteFile failed after {written_total} bytes (Win32 error {})",
                        win::last_error()
                    ));
                }
                if written == 0 {
                    return Err("WriteFile reported 0 bytes written".to_string());
                }
                written_total += written as usize;
            }
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            return self
                .file
                .write_all(data)
                .map_err(|e| format!("write failed: {e}"));
        }
    }

    /// Truncates (or extends) the file to exactly `size` bytes:
    /// `SetFilePointerEx` to the size, then `SetEndOfFile`. This is how an
    /// unbuffered run — whose last write was padded up to a sector boundary
    /// — ends at the exact requested byte.
    fn truncate_to(&mut self, size: u64) -> Result<(), String> {
        #[cfg(windows)]
        unsafe {
            let mut pos: i64 = 0;
            if win::SetFilePointerEx(self.handle, size as i64, &mut pos, win::FILE_BEGIN) == 0 {
                return Err(format!(
                    "SetFilePointerEx failed (Win32 error {})",
                    win::last_error()
                ));
            }
            if win::SetEndOfFile(self.handle) == 0 {
                return Err(format!(
                    "SetEndOfFile failed (Win32 error {})",
                    win::last_error()
                ));
            }
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            return self
                .file
                .set_len(size)
                .map_err(|e| format!("truncate failed: {e}"));
        }
    }

    fn flush(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = win::FlushFileBuffers(self.handle);
        }
        #[cfg(not(windows))]
        {
            let _ = self.file.sync_all();
        }
    }
}

#[cfg(windows)]
impl Drop for OutputFile {
    fn drop(&mut self) {
        unsafe {
            win::CloseHandle(self.handle);
        }
    }
}

// ===========================================================================
// Section 8 — The generator engine
// ===========================================================================
//
// Generation is allocation-free after construction: line-oriented profiles
// build one line at a time into a reusable staging buffer and copy out of
// it (so a line may span any number of write blocks), while the stream
// profiles (infinite-line, dirty-hex) fill the write block directly.
//
// Word tables for the synthetic-source and filler profiles. These are
// realistic fragments — the point is data whose token, brace, and string
// shapes make an editor's tokenizer and piece table do real work.

const CODE_TYPES: &[&str] = &[
    "int",
    "u32",
    "u64",
    "i64",
    "bool",
    "float",
    "double",
    "String",
    "Vec<u8>",
    "auto",
    "size_t",
    "char",
    "void",
    "Result<T>",
    "Option<&str>",
    "u8",
    "i32",
    "f64",
];
const CODE_VARS: &[&str] = &[
    "count",
    "index",
    "buffer",
    "offset",
    "total",
    "value",
    "result",
    "node",
    "cursor",
    "length",
    "width",
    "height",
    "token",
    "state",
    "cache",
    "window",
    "start",
    "end",
    "chunk",
    "line",
    "bytes_read",
    "pending",
    "active",
    "current",
    "next_pos",
    "glyph",
    "piece",
    "table",
    "span",
];
const CODE_FUNCS: &[&str] = &[
    "parse_line",
    "render",
    "flush",
    "compute_layout",
    "find_next",
    "insert_piece",
    "split_at",
    "measure_text",
    "load_block",
    "validate",
    "normalize",
    "advance",
    "lookup",
    "build_index",
    "apply_edit",
    "scroll_to",
    "hit_test",
    "encode",
    "decode_block",
    "refresh_view",
];
const CODE_WORDS: &[&str] = &[
    "the",
    "editor",
    "must",
    "handle",
    "this",
    "without",
    "allocating",
    "per",
    "line",
    "because",
    "files",
    "grow",
    "beyond",
    "memory",
    "and",
    "every",
    "byte",
    "counts",
    "towards",
    "latency",
    "scrolling",
    "search",
    "wrapping",
    "selection",
    "rendering",
    "indexing",
    "parsing",
    "caching",
];
const CSV_NAMES: &[&str] = &[
    "Ada Lovelace",
    "Alan Turing",
    "Grace Hopper",
    "Edsger Dijkstra",
    "Barbara Liskov",
    "Donald Knuth",
    "Margaret Hamilton",
    "Katherine Johnson",
    "Radia Perlman",
    "Tim Berners-Lee",
    "Dennis Ritchie",
    "Ken Thompson",
    "Linus Torvalds",
    "Guido van Rossum",
    "Bjarne Stroustrup",
];
const CSV_CITIES: &[&str] = &[
    "Chicago",
    "Austin",
    "Denver",
    "Seattle",
    "Portland",
    "Boston",
    "Miami",
    "Phoenix",
    "Nashville",
    "Detroit",
    "San Jose",
    "Raleigh",
    "Boise",
    "Tulsa",
    "Fresno",
];
/// UTF-8 token table for unicode-stress: CJK, kana, hangul, Arabic, Hebrew,
/// Devanagari, emoji, combining sequences, a Zalgo stack, and the raw bytes
/// of the U+202E RIGHT-TO-LEFT OVERRIDE and U+200F RIGHT-TO-LEFT MARK.
const UNICODE_TOKENS: &[&str] = &[
    "hello", "world", "世界", "你好世界", "日本語テスト", "中文测试数据", "한국어시험",
    "😀", "🚀", "🔥✨", "🎉", "مرحبا بالعالم", "שלום עולם", "नमस्ते दुनिया",
    "e\u{301}", "a\u{308}\u{304}", "o\u{303}\u{301}\u{342}",
    "Z\u{330}\u{355}\u{342}\u{317}\u{349}\u{33e}a\u{335}\u{34d}\u{31c}l\u{338}\u{353}g\u{31e}\u{339}o\u{31d}",
    "\u{202e}reversed\u{202c}", "\u{200f}marked", "Ωμέγα", "Привет", "café", "naïve",
    "∑∫π√", "→←↑↓", "①②③", "Ｆｕｌｌｗｉｄｔｈ", "\u{0}P", "crème brûlée",
];
const FILLER_SENTENCES: &[&str] = &[
    "The quick brown fox jumps over the lazy dog. ",
    "Pack my box with five dozen liquor jugs. ",
    "How vexingly quick daft zebras jump! ",
    "The five boxing wizards jump quickly. ",
    "Sphinx of black quartz, judge my vow. ",
    "Bright vixens jump; dozy fowl quack. ",
];

struct Generator {
    rng: Xoshiro256PlusPlus,
    profile: Profile,
    line_min: usize,
    line_max: usize,
    line_len_custom: bool,
    eol: Eol,
    brace_depth: u32,
    generated_line_number: u64,
    stage: Vec<u8>,
    stage_len: usize,
    stage_pos: usize,
}

impl Generator {
    fn new(cfg: &Config) -> Generator {
        // Staging must hold the longest possible line plus its terminator.
        // ragged-csv can reach 8000 chars by default; a custom --line-len
        // can ask for more, so size from the configured maximum.
        let stage_cap = std::cmp::max(16_384, cfg.line_max.saturating_mul(2).saturating_add(1024));
        Generator {
            rng: Xoshiro256PlusPlus::from_seed(cfg.seed),
            profile: cfg.profile,
            line_min: cfg.line_min,
            line_max: cfg.line_max,
            line_len_custom: cfg.line_len_custom,
            eol: cfg.eol,
            brace_depth: 0,
            generated_line_number: 0,
            stage: vec![0u8; stage_cap],
            stage_len: 0,
            stage_pos: 0,
        }
    }

    /// Fills `out` completely with generated bytes.
    fn fill(&mut self, out: &mut [u8]) {
        match self.profile {
            Profile::InfiniteLine => self.fill_infinite_line(out),
            Profile::DirtyHex => self.fill_dirty_hex(out),
            _ => self.fill_from_stage(out),
        }
    }

    // -- staging plumbing ---------------------------------------------------

    fn stage_push(&mut self, bytes: &[u8]) {
        let room = self.stage.len() - self.stage_len;
        let take = std::cmp::min(room, bytes.len());
        self.stage[self.stage_len..self.stage_len + take].copy_from_slice(&bytes[..take]);
        self.stage_len += take;
    }
    fn stage_push_byte(&mut self, b: u8) {
        if self.stage_len < self.stage.len() {
            self.stage[self.stage_len] = b;
            self.stage_len += 1;
        }
    }
    fn stage_push_str(&mut self, s: &str) {
        self.stage_push(s.as_bytes());
    }
    fn stage_push_repeat(&mut self, b: u8, count: usize) {
        for _ in 0..count {
            self.stage_push_byte(b);
        }
    }
    fn stage_push_u64(&mut self, mut value: u64) {
        // Decimal, written without formatting machinery on the hot path.
        let mut digits = [0u8; 20];
        let mut n = 0usize;
        if value == 0 {
            self.stage_push_byte(b'0');
            return;
        }
        while value > 0 {
            digits[n] = b'0' + (value % 10) as u8;
            value /= 10;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            self.stage_push_byte(digits[n]);
        }
    }
    fn stage_finish_line(&mut self) {
        let mut eol_buf = [0u8; 2];
        let eol_len = {
            let eol = self.eol;
            eol.write_into(&mut self.rng, &mut eol_buf)
        };
        self.stage_push(&eol_buf[..eol_len]);
        self.generated_line_number += 1;
    }

    /// Finishes a line after enforcing a user-supplied `--line-len` range:
    /// short lines are padded with spaces to the minimum, long lines are
    /// truncated to the maximum (on a UTF-8 boundary, so unicode-stress
    /// never emits a torn multi-byte sequence). Without a custom range the
    /// natural per-profile shapes are left untouched — real code has short
    /// brace lines, and real CSV rows are ragged.
    fn stage_finish_line_enforced(&mut self) {
        if self.line_len_custom {
            if self.stage_len < self.line_min {
                let pad = self.line_min - self.stage_len;
                self.stage_push_repeat(b' ', pad);
            }
            if self.stage_len > self.line_max {
                let mut cut = self.line_max;
                while cut > 0 && (self.stage[cut] & 0xC0) == 0x80 {
                    cut -= 1;
                }
                self.stage_len = cut;
            }
        }
        self.stage_finish_line();
    }

    fn fill_from_stage(&mut self, out: &mut [u8]) {
        let mut written = 0usize;
        while written < out.len() {
            if self.stage_pos >= self.stage_len {
                self.stage_len = 0;
                self.stage_pos = 0;
                self.generate_line();
                if self.stage_len == 0 {
                    // Defensive: a generator bug must never spin forever.
                    self.stage_push_byte(b' ');
                    self.stage_len = 1;
                }
            }
            let available = self.stage_len - self.stage_pos;
            let take = std::cmp::min(available, out.len() - written);
            out[written..written + take]
                .copy_from_slice(&self.stage[self.stage_pos..self.stage_pos + take]);
            self.stage_pos += take;
            written += take;
        }
    }

    fn generate_line(&mut self) {
        match self.profile {
            Profile::DenseCode => self.gen_dense_code_line(),
            Profile::RaggedCsv => self.gen_ragged_csv_line(),
            Profile::UnicodeStress => self.gen_unicode_line(),
            Profile::NeedleBench => self.gen_needle_bench_line(),
            Profile::InfiniteLine | Profile::DirtyHex => {
                // Stream profiles never reach line generation.
                self.stage_push_byte(b'x');
            }
        }
    }

    fn target_line_len(&mut self) -> usize {
        let lo = self.line_min.max(1);
        let hi = self.line_max.max(lo);
        self.rng.range_inclusive(lo as u64, hi as u64) as usize
    }

    // -- dense-code -----------------------------------------------------------

    fn gen_dense_code_line(&mut self) {
        let target = self.target_line_len();
        let roll = self.rng.below(100);
        let indent = (self.brace_depth.min(5) * 4) as usize;

        if roll < 7 && self.brace_depth > 0 {
            // Closing brace, sometimes with a comment.
            self.brace_depth -= 1;
            let ind = (self.brace_depth.min(5) * 4) as usize;
            self.stage_push_repeat(b' ', ind);
            self.stage_push_byte(b'}');
            if self.rng.below(3) == 0 {
                self.stage_push_str(" // end ");
                {
                    let v = *self.rng.pick(CODE_FUNCS);
                    self.stage_push_str(v);
                }
            }
            self.stage_finish_line_enforced();
            return;
        }
        self.stage_push_repeat(b' ', indent);
        match roll {
            7..=16 => {
                // Opening construct: pushes the brace depth up.
                match self.rng.below(4) {
                    0 => {
                        self.stage_push_str("if (");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(" > ");
                        {
                            let v = self.rng.below(1000);
                            self.stage_push_u64(v);
                        }
                        self.stage_push_str(") {");
                    }
                    1 => {
                        self.stage_push_str("for (size_t i = 0; i < ");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(".size(); ++i) {");
                    }
                    2 => {
                        self.stage_push_str("fn ");
                        {
                            let v = *self.rng.pick(CODE_FUNCS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str("(");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(": ");
                        {
                            let v = *self.rng.pick(CODE_TYPES);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(") -> bool {");
                    }
                    _ => {
                        self.stage_push_str("while (");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(" != nullptr) {");
                    }
                }
                self.brace_depth = (self.brace_depth + 1).min(6);
            }
            17..=26 => {
                // Comment line padded with realistic prose toward the target.
                self.stage_push_str("// ");
                while self.stage_len + 6 < target {
                    {
                        let v = *self.rng.pick(CODE_WORDS);
                        self.stage_push_str(v);
                    }
                    self.stage_push_byte(b' ');
                }
            }
            27..=36 => {
                // Declaration carrying a string literal.
                {
                    let v = if self.rng.below(2) == 0 {
                        "let "
                    } else {
                        "auto "
                    };
                    self.stage_push_str(v);
                }
                {
                    let v = *self.rng.pick(CODE_VARS);
                    self.stage_push_str(v);
                }
                self.stage_push_str(" = \"");
                let words = self.rng.range_inclusive(1, 5);
                for w in 0..words {
                    if w > 0 {
                        self.stage_push_byte(b' ');
                    }
                    {
                        let v = *self.rng.pick(CODE_WORDS);
                        self.stage_push_str(v);
                    }
                }
                if self.rng.below(4) == 0 {
                    self.stage_push_str("\\n\\t\\\"escaped\\\"");
                }
                self.stage_push_str("\";");
            }
            37..=46 => {
                // Plain declaration.
                {
                    let v = *self.rng.pick(CODE_TYPES);
                    self.stage_push_str(v);
                }
                self.stage_push_byte(b' ');
                {
                    let v = *self.rng.pick(CODE_VARS);
                    self.stage_push_str(v);
                }
                self.stage_push_str(" = ");
                {
                    let v = self.rng.below(1_000_000);
                    self.stage_push_u64(v);
                }
                self.stage_push_byte(b';');
            }
            _ => {
                // Assignment / call / return statement.
                match self.rng.below(3) {
                    0 => {
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(" = ");
                        {
                            let v = *self.rng.pick(CODE_FUNCS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_byte(b'(');
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(", ");
                        {
                            let v = self.rng.below(4096);
                            self.stage_push_u64(v);
                        }
                        self.stage_push_str(");")
                    }
                    1 => {
                        self.stage_push_str("return ");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(" + ");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_byte(b';')
                    }
                    _ => {
                        {
                            let v = *self.rng.pick(CODE_FUNCS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str("(&");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(", ");
                        {
                            let v = *self.rng.pick(CODE_VARS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_str(");")
                    }
                }
                // Grow short statements toward the target with a trailing
                // comment, which real code does constantly.
                if self.stage_len + 10 < target {
                    self.stage_push_str(" // ");
                    while self.stage_len + 6 < target {
                        {
                            let v = *self.rng.pick(CODE_WORDS);
                            self.stage_push_str(v);
                        }
                        self.stage_push_byte(b' ');
                    }
                }
            }
        }
        self.stage_finish_line_enforced();
    }

    // -- ragged-csv -----------------------------------------------------------

    fn gen_ragged_csv_line(&mut self) {
        // Delimiter: mostly commas, sometimes a tab (TSV rows in a CSV file
        // are exactly the kind of raggedness this profile exists to create).
        let delim: u8 = if self.rng.below(100) < 85 {
            b','
        } else {
            b'\t'
        };
        // Target length distribution: many short rows, a long tail of very
        // long ones (up to and past 8000 chars when not customised).
        let target: usize = if self.line_len_custom {
            self.target_line_len()
        } else {
            match self.rng.below(100) {
                0..=59 => self.rng.range_inclusive(10, 120) as usize,
                60..=84 => self.rng.range_inclusive(120, 600) as usize,
                85..=96 => self.rng.range_inclusive(600, 2500) as usize,
                _ => self.rng.range_inclusive(2500, 8000) as usize,
            }
        };
        let max_columns = self.rng.range_inclusive(1, 16);
        let mut columns = 0u64;
        loop {
            if columns > 0 {
                self.stage_push_byte(delim);
            }
            self.gen_csv_field(delim);
            columns += 1;
            if columns >= max_columns && self.stage_len >= target / 2 {
                break;
            }
            if self.stage_len >= target {
                break;
            }
            if columns >= 64 {
                break; // hard stop: a row must always terminate
            }
        }
        self.stage_finish_line_enforced();
    }

    fn gen_csv_field(&mut self, _delim: u8) {
        match self.rng.below(10) {
            0 => {
                // Integer.
                if self.rng.below(5) == 0 {
                    self.stage_push_byte(b'-');
                }
                {
                    let v = self.rng.below(1_000_000_000);
                    self.stage_push_u64(v);
                }
            }
            1 => {
                // Decimal: whole '.' two digits, no formatter needed.
                {
                    let v = self.rng.below(100_000);
                    self.stage_push_u64(v);
                }
                self.stage_push_byte(b'.');
                let frac = self.rng.below(100);
                self.stage_push_byte(b'0' + (frac / 10) as u8);
                self.stage_push_byte(b'0' + (frac % 10) as u8);
            }
            2 => {
                let v = *self.rng.pick(CSV_NAMES);
                self.stage_push_str(v);
            }
            3 => {
                let v = *self.rng.pick(CSV_CITIES);
                self.stage_push_str(v);
            }
            4 => {
                // Date, hand-formatted.
                {
                    let v = self.rng.range_inclusive(1990, 2026);
                    self.stage_push_u64(v);
                }
                self.stage_push_byte(b'-');
                let m = self.rng.range_inclusive(1, 12);
                self.stage_push_byte(b'0' + (m / 10) as u8);
                self.stage_push_byte(b'0' + (m % 10) as u8);
                self.stage_push_byte(b'-');
                let d = self.rng.range_inclusive(1, 28);
                self.stage_push_byte(b'0' + (d / 10) as u8);
                self.stage_push_byte(b'0' + (d % 10) as u8);
            }
            5 => {
                // Quoted field with embedded delimiters and escaped quotes:
                // a doubled "" inside a quoted field is one literal quote.
                self.stage_push_byte(b'"');
                let words = self.rng.range_inclusive(1, 6);
                for w in 0..words {
                    if w > 0 {
                        self.stage_push_byte(b' ');
                    }
                    {
                        let v = *self.rng.pick(CODE_WORDS);
                        self.stage_push_str(v);
                    }
                    if self.rng.below(4) == 0 {
                        self.stage_push_str(", still quoted");
                    }
                    if self.rng.below(5) == 0 {
                        self.stage_push_str("\"\"");
                    }
                }
                self.stage_push_byte(b'"');
            }
            6 => {
                // Empty field.
            }
            7 => {
                let v = if self.rng.below(2) == 0 {
                    "true"
                } else {
                    "false"
                };
                self.stage_push_str(v);
            }
            8 => {
                // Email-shaped field.
                {
                    let v = *self.rng.pick(CODE_VARS);
                    self.stage_push_str(v);
                }
                self.stage_push_byte(b'@');
                {
                    let city = *self.rng.pick(CSV_CITIES);
                    for b in city.bytes() {
                        self.stage_push_byte(b.to_ascii_lowercase());
                    }
                }
                self.stage_push_str(".example.com");
            }
            _ => {
                // Long free-text field: this is what stretches some rows to
                // thousands of characters.
                let extra = self.rng.range_inclusive(20, 400);
                for _ in 0..extra {
                    {
                        let v = *self.rng.pick(CODE_WORDS);
                        self.stage_push_str(v);
                    }
                    self.stage_push_byte(b' ');
                    if self.stage_len >= self.stage.len() - 64 {
                        break;
                    }
                }
            }
        }
    }

    // -- unicode-stress ---------------------------------------------------------

    fn gen_unicode_line(&mut self) {
        let target = self.target_line_len();
        if self.rng.below(6) == 0 {
            self.stage_push_str("    ");
        }
        while self.stage_len < target {
            {
                let v = *self.rng.pick(UNICODE_TOKENS);
                self.stage_push_str(v);
            }
            // Separators are mostly spaces; combining tokens already carry
            // their own marks, so no extra work is needed here.
            if self.rng.below(5) != 0 {
                self.stage_push_byte(b' ');
            }
            if self.stage_len >= self.stage.len() - 128 {
                break;
            }
        }
        self.stage_finish_line_enforced();
    }

    // -- needle-bench background ------------------------------------------------

    fn gen_needle_bench_line(&mut self) {
        // Predictable background: a running line number, then filler
        // sentences. Needles are stamped over this stream later, at exact
        // configured byte offsets, by the writer loop.
        let target = self.target_line_len();
        self.stage_push_u64(self.generated_line_number + 1);
        self.stage_push_str(" | ");
        while self.stage_len < target {
            {
                let v = *self.rng.pick(FILLER_SENTENCES);
                self.stage_push_str(v);
            }
            if self.stage_len >= self.stage.len() - 128 {
                break;
            }
        }
        self.stage_finish_line_enforced();
    }

    // -- infinite-line ----------------------------------------------------------

    #[allow(clippy::chunks_exact_to_as_chunks)]
    fn fill_infinite_line(&mut self, out: &mut [u8]) {
        // Printable ASCII with no CR or LF anywhere — one line that never
        // ends, which is what breaks naive line-offset tables.
        const CHARSET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-.=+/()[]{}<>;:#@$%!*&|";
        let n = CHARSET.len() as u64;
        let mut chunks = out.chunks_exact_mut(8);
        for c in &mut chunks {
            let v = self.rng.next_u64();
            for (i, byte) in c.iter_mut().enumerate() {
                let idx = ((v >> (i * 8)) & 0xFF) % n;
                *byte = CHARSET[idx as usize];
            }
        }
        let rem = chunks.into_remainder();
        if !rem.is_empty() {
            let v = self.rng.next_u64();
            for (i, byte) in rem.iter_mut().enumerate() {
                let idx = ((v >> (i * 8)) & 0xFF) % n;
                *byte = CHARSET[idx as usize];
            }
        }
    }

    // -- dirty-hex ----------------------------------------------------------------

    fn fill_dirty_hex(&mut self, out: &mut [u8]) {
        // Base layer: raw PRNG bytes (already ~0.4% nulls by chance).
        self.rng.fill_bytes(out);
        // Sprinkled structures, at irregular intervals, so binary-vs-text
        // heuristics, UTF-8 validators, and hex views all meet edge cases
        // inside a single file: valid UTF-8 fragments, NUL runs, the UTF-8
        // encoding of an orphaned surrogate (ED A0 80, invalid), UTF-16 BOM
        // fragments, and ANSI terminal escapes.
        const PATTERNS: &[&[u8]] = &[
            b"\x00\x00\x00",
            b"\x1b[31m",
            b"\x1b[0m\x1b[K",
            "héllo wörld ".as_bytes(),
            b"\xed\xa0\x80",
            b"\xff\xfeH\x00i\x00",
            b"Hello, world! ",
            b"\x00",
            b"\xc3\x28", // invalid UTF-8 sequence
            b"\xe2\x82", // truncated 3-byte sequence start
            b"\x07\x08\x0b\x0c\x7f",
        ];
        let mut pos = self.rng.below(64) as usize;
        while pos < out.len() {
            let pattern = *self.rng.pick(PATTERNS);
            let take = std::cmp::min(pattern.len(), out.len() - pos);
            out[pos..pos + take].copy_from_slice(&pattern[..take]);
            pos += self.rng.range_inclusive(53, 211) as usize;
        }
    }
}

// ===========================================================================
// Section 9 — Post-generation transforms: needle stamping and null forcing
// ===========================================================================

/// One recorded needle placement, for the `.index` sidecar.
struct NeedleHit {
    byte_offset: u64,
    line_number: u64, // 1-based
}

/// Stamps every needle whose start offset falls inside the block that
/// begins at `block_start` over the freshly generated bytes, recording
/// each hit. Needles may straddle a block boundary: the part inside this
/// block is written now, and the remainder is written when the next block
/// is processed (the hit is recorded once, at its start).
///
/// Line numbers are counted against the final file: `lines_before_block`
/// is the number of `\n` bytes in all previous blocks (after their own
/// stamping), and newlines inside this block before the needle start are
/// counted on the pre-stamp bytes — a needle never contains a newline the
/// caller depends on unless the needle itself does, in which case the
/// recorded line is the line the needle *starts* on, which is the useful
/// assertion value either way.
fn stamp_needles(
    block: &mut [u8],
    block_start: u64,
    lines_before_block: u64,
    needle: &[u8],
    needle_every: u64,
    hits: &mut Vec<NeedleHit>,
    carry: &mut usize, // bytes of a straddling needle still owed to this block
) {
    if needle.is_empty() || needle_every == 0 {
        return;
    }
    let block_end = block_start + block.len() as u64;
    let mut offset_in_block = 0usize;

    // Finish a needle that started in the previous block.
    if *carry > 0 {
        let take = std::cmp::min(*carry, block.len());
        let start = needle.len() - *carry;
        block[..take].copy_from_slice(&needle[start..start + take]);
        *carry -= take;
        offset_in_block = take;
        if *carry > 0 {
            return;
        }
    }

    // First needle start at or after block_start.
    let mut next = if block_start == 0 {
        0
    } else {
        // Smallest multiple of needle_every that is >= block_start, careful
        // to also consider a needle that starts exactly at block_start when
        // the previous block ended mid-interval.
        let rem = block_start % needle_every;
        if rem == 0 {
            block_start
        } else {
            block_start + (needle_every - rem)
        }
    };
    // A needle starting before this block but not flagged as carry cannot
    // happen: carry is set whenever a start lands in the previous block.
    while next < block_end {
        let start_in_block = (next - block_start) as usize;
        if start_in_block < offset_in_block {
            next += needle_every;
            continue;
        }
        // Line number: lines fully before this block, plus newlines in this
        // block before the needle, plus one (lines are 1-based).
        let newlines_before = block[..start_in_block]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u64;
        hits.push(NeedleHit {
            byte_offset: next,
            line_number: lines_before_block + newlines_before + 1,
        });
        let available = block.len() - start_in_block;
        let take = std::cmp::min(available, needle.len());
        block[start_in_block..start_in_block + take].copy_from_slice(&needle[..take]);
        if take < needle.len() {
            *carry = needle.len() - take;
        }
        next += needle_every;
    }
}

/// Forces a percentage of the block's bytes to 0x00. Costs a PRNG draw per
/// byte, so it only runs when `--null-rate` is above zero.
fn apply_null_rate(rng: &mut Xoshiro256PlusPlus, block: &mut [u8], pct: f64) {
    if pct <= 0.0 {
        return;
    }
    if pct >= 100.0 {
        block.fill(0);
        return;
    }
    for byte in block.iter_mut() {
        if rng.chance_pct(pct) {
            *byte = 0;
        }
    }
}

/// Counts `\n` bytes — the line counter the index and nothing else uses.
fn count_newlines(block: &[u8]) -> u64 {
    block.iter().filter(|&&b| b == b'\n').count() as u64
}

// ===========================================================================
// Section 10 — Human-readable formatting for progress and the summary
// ===========================================================================

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

fn format_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB/s", bytes_per_sec / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.1} MB/s", bytes_per_sec / (1024.0 * 1024.0))
    }
}

fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".to_string();
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

// ===========================================================================
// Section 11 — The write loop, progress reporting, sidecar, and summary
// ===========================================================================

struct RunSummary {
    bytes_written: u64,
    elapsed_secs: f64,
    sha256_hex: String,
    needle_count: usize,
    index_path: Option<String>,
}

fn run(cfg: &Config) -> Result<RunSummary, String> {
    let timer = PerfTimer::new();
    // Unbuffered writes must be sector multiples in offset and length, so
    // the physical stream is padded up to a sector boundary and truncated
    // back to the exact size at the end.
    let padded_total = if cfg.unbuffered {
        round_up(cfg.size, SECTOR_SIZE)
    } else {
        cfg.size
    };

    let mut output = OutputFile::create(&cfg.output_path, cfg.unbuffered, padded_total)?;
    let mut block = AlignedBlock::new(BLOCK_SIZE)?;
    let mut generator = Generator::new(cfg);
    let mut sha = Sha256::new();

    let mut hits: Vec<NeedleHit> = Vec::new();
    let mut needle_carry: usize = 0;
    let mut lines_before_block: u64 = 0;

    let mut generated: u64 = 0;
    let mut last_print_at = f64::NEG_INFINITY;
    let mut last_print_bytes: u64 = 0;
    let mut smoothed_rate: f64 = 0.0;

    eprintln!(
        "Generating {} ({}, seed 0x{:016x}, eol {}, unbuffered {}) -> {}",
        format_bytes(cfg.size),
        cfg.profile.name(),
        cfg.seed,
        cfg.eol.name(),
        cfg.unbuffered,
        cfg.output_path
    );

    while generated < cfg.size {
        let remaining = cfg.size - generated;
        let logical = std::cmp::min(remaining, BLOCK_SIZE as u64) as usize;
        let write_len = if cfg.unbuffered {
            round_up(logical as u64, SECTOR_SIZE) as usize
        } else {
            logical
        };

        {
            let buf = block.as_mut_slice();
            generator.fill(&mut buf[..logical]);
            if cfg.null_rate > 0.0 {
                // Null forcing runs before needle stamping so a needle is
                // never erased by the null pass: needles stay findable,
                // which is the entire point of recording them.
                apply_null_rate(&mut generator.rng, &mut buf[..logical], cfg.null_rate);
            }
            if let Some(needle) = &cfg.needle {
                stamp_needles(
                    &mut buf[..logical],
                    generated,
                    lines_before_block,
                    needle,
                    cfg.needle_every,
                    &mut hits,
                    &mut needle_carry,
                );
            }
            lines_before_block += count_newlines(&buf[..logical]);
            sha.update(&buf[..logical]);
            if write_len > logical {
                buf[logical..write_len].fill(0);
            }
        }
        output.write_all(&block.as_slice()[..write_len])?;
        generated += logical as u64;

        // Throttled progress: at most once every 100 ms, plus the final
        // state, so the console never becomes the bottleneck.
        let now = timer.elapsed_secs();
        if now - last_print_at >= 0.100 || generated == cfg.size {
            let dt = now - last_print_at;
            if dt > 0.0 && last_print_at != f64::NEG_INFINITY {
                let instant = (generated - last_print_bytes) as f64 / dt;
                smoothed_rate = if smoothed_rate == 0.0 {
                    instant
                } else {
                    smoothed_rate * 0.6 + instant * 0.4
                };
            }
            last_print_at = now;
            last_print_bytes = generated;
            print_progress(cfg.size, generated, now, smoothed_rate);
        }
    }

    // Exact final size, undoing both the sector padding and any
    // pre-allocation extension; then make sure the bytes are on disk.
    output.truncate_to(cfg.size)?;
    output.flush();
    drop(output);

    let elapsed = timer.elapsed_secs();

    // Needle sidecar: `<output>.index`, tab-separated so a test harness can
    // read it with a plain split. Offsets are byte offsets into the output
    // file; line numbers are 1-based.
    let index_path = if cfg.needle.is_some() {
        let path = format!("{}.index", cfg.output_path);
        let needle_text = String::from_utf8_lossy(cfg.needle.as_deref().unwrap_or(&[]));
        let mut text = String::new();
        text.push_str("# largetxt-gen needle index v1\n");
        text.push_str(&format!("# needle: {needle_text}\n"));
        text.push_str(&format!("# needle_every_bytes: {}\n", cfg.needle_every));
        text.push_str("# byte_offset\tline_number\tneedle\n");
        for hit in &hits {
            text.push_str(&format!(
                "{}\t{}\t{}\n",
                hit.byte_offset, hit.line_number, needle_text
            ));
        }
        std::fs::write(&path, text)
            .map_err(|e| format!("cannot write needle index '{path}': {e}"))?;
        Some(path)
    } else {
        None
    };

    eprintln!(); // end the \r progress line
    Ok(RunSummary {
        bytes_written: cfg.size,
        elapsed_secs: elapsed,
        sha256_hex: sha.finalize_hex(),
        needle_count: hits.len(),
        index_path,
    })
}

/// One throttled progress line, rewritten in place with `\r`.
fn print_progress(total: u64, done: u64, elapsed: f64, inst_rate: f64) {
    let pct = if total == 0 {
        100.0
    } else {
        done as f64 * 100.0 / total as f64
    };
    let bar_width = 30usize;
    let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
    let bar: String = "#".repeat(filled) + &"-".repeat(bar_width - filled.min(bar_width));
    let avg_rate = if elapsed > 0.0 {
        done as f64 / elapsed
    } else {
        0.0
    };
    let shown_rate = if inst_rate > 0.0 { inst_rate } else { avg_rate };
    let eta = if shown_rate > 0.0 && done < total {
        (total - done) as f64 / shown_rate
    } else {
        0.0
    };
    eprint!(
        "\r[{bar}] {pct:6.2}%  {} / {}  {}  avg {}  elapsed {}  ETA {}   ",
        format_bytes(done),
        format_bytes(total),
        format_rate(shown_rate),
        format_rate(avg_rate),
        format_duration(elapsed),
        format_duration(eta)
    );
    let _ = std::io::stderr().flush();
}

fn main() {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err((msg, code)) => {
            if code == 0 {
                // --help already printed itself; --version carries its text.
                print!("{msg}");
            } else {
                eprint!("{msg}");
            }
            std::process::exit(code);
        }
    };

    match run(&cfg) {
        Ok(summary) => {
            let avg = if summary.elapsed_secs > 0.0 {
                summary.bytes_written as f64 / summary.elapsed_secs
            } else {
                0.0
            };
            println!("Done: {}", cfg.output_path);
            println!(
                "Profile: {} | seed: 0x{:016x} | eol: {}",
                cfg.profile.name(),
                cfg.seed,
                cfg.eol.name()
            );
            println!(
                "Bytes written: {} ({})",
                summary.bytes_written,
                format_bytes(summary.bytes_written)
            );
            println!("Elapsed: {}", format_duration(summary.elapsed_secs));
            println!("Average throughput: {}", format_rate(avg));
            println!("SHA-256: {}", summary.sha256_hex);
            if let Some(index) = &summary.index_path {
                println!(
                    "Needles: {} placement(s), index: {}",
                    summary.needle_count, index
                );
            }
        }
        Err(e) => {
            eprintln!("\nerror: {e}");
            std::process::exit(2);
        }
    }
}

// ===========================================================================
// Section 12 — Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg(profile: Profile, seed: u64) -> Config {
        let (line_min, line_max) = profile.default_line_range();
        Config {
            output_path: "unused".to_string(),
            size: 0,
            profile,
            line_min,
            line_max,
            line_len_custom: false,
            eol: Eol::Crlf,
            needle: None,
            needle_every: DEFAULT_NEEDLE_EVERY,
            null_rate: 0.0,
            unbuffered: true,
            seed,
        }
    }

    fn generate(profile: Profile, seed: u64, bytes: usize) -> Vec<u8> {
        let cfg = test_cfg(profile, seed);
        let mut gen = Generator::new(&cfg);
        let mut out = vec![0u8; bytes];
        gen.fill(&mut out);
        out
    }

    #[test]
    fn size_parser_binary_suffixes() {
        assert_eq!(parse_size("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("10G"), Some(10 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("1T"), Some(1024u64.pow(4)));
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("64KB"), Some(64 * 1024));
        assert_eq!(parse_size("2k"), Some(2048));
        assert_eq!(parse_size("1.5G"), Some(1610612736));
        assert_eq!(parse_size("0"), Some(0));
    }

    #[test]
    fn size_parser_rejects_garbage() {
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("12X"), None);
        assert_eq!(parse_size("M"), None);
        assert_eq!(parse_size("-5M"), None);
        assert_eq!(parse_size("999999999999999999999T"), None);
        assert_eq!(parse_size("1.2.3G"), None);
    }

    #[test]
    fn seed_parser_is_hex() {
        assert_eq!(parse_seed("0x10"), Some(16));
        assert_eq!(parse_seed("FF"), Some(255));
        assert_eq!(parse_seed("10"), Some(16)); // hex per the flag contract
        assert_eq!(parse_seed("0XdeadBEEF"), Some(0xdead_beef));
        assert_eq!(parse_seed("xyz"), None);
        assert_eq!(parse_seed(""), None);
        assert_eq!(parse_seed("0x"), None);
    }

    #[test]
    fn line_len_parser() {
        assert_eq!(parse_line_len("80:120"), Some((80, 120)));
        assert_eq!(parse_line_len("5:5"), Some((5, 5)));
        assert_eq!(parse_line_len("120:80"), None);
        assert_eq!(parse_line_len("0:10"), None);
        assert_eq!(parse_line_len("80"), None);
    }

    #[test]
    fn round_up_to_sector() {
        assert_eq!(round_up(0, SECTOR_SIZE), 0);
        assert_eq!(round_up(1, SECTOR_SIZE), 4096);
        assert_eq!(round_up(4096, SECTOR_SIZE), 4096);
        assert_eq!(round_up(4097, SECTOR_SIZE), 8192);
    }

    #[test]
    fn prng_is_deterministic() {
        let mut a = Xoshiro256PlusPlus::from_seed(42);
        let mut b = Xoshiro256PlusPlus::from_seed(42);
        let mut c = Xoshiro256PlusPlus::from_seed(43);
        let va: Vec<u64> = (0..100).map(|_| a.next_u64()).collect();
        let vb: Vec<u64> = (0..100).map(|_| b.next_u64()).collect();
        let vc: Vec<u64> = (0..100).map(|_| c.next_u64()).collect();
        assert_eq!(va, vb);
        assert_ne!(va, vc);
        assert!(va.iter().any(|&x| x != 0));
    }

    #[test]
    fn sha256_known_vectors() {
        let mut h = Sha256::new();
        h.update(b"");
        assert_eq!(
            h.finalize_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            h.finalize_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // One million 'a' bytes, fed in awkward chunk sizes to prove the
        // streaming buffer path.
        let mut h = Sha256::new();
        let chunk = [b'a'; 997];
        let mut fed = 0usize;
        while fed < 1_000_000 {
            let take = std::cmp::min(chunk.len(), 1_000_000 - fed);
            h.update(&chunk[..take]);
            fed += take;
        }
        assert_eq!(
            h.finalize_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn generator_is_deterministic_per_profile() {
        for profile in [
            Profile::DenseCode,
            Profile::InfiniteLine,
            Profile::RaggedCsv,
            Profile::DirtyHex,
            Profile::UnicodeStress,
            Profile::NeedleBench,
        ] {
            let a = generate(profile, 7, 65_536);
            let b = generate(profile, 7, 65_536);
            let c = generate(profile, 8, 65_536);
            assert_eq!(a, b, "{} not deterministic", profile.name());
            assert_ne!(a, c, "{} ignores its seed", profile.name());
        }
    }

    #[test]
    fn infinite_line_has_no_newlines() {
        let out = generate(Profile::InfiniteLine, 99, 262_144);
        assert!(!out.contains(&b'\n'));
        assert!(!out.contains(&b'\r'));
        assert!(out.iter().all(|b| b.is_ascii_graphic()));
    }

    #[test]
    fn dense_code_looks_like_code() {
        let out = generate(Profile::DenseCode, 5, 262_144);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("\r\n"), "CRLF line endings expected");
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert!(lines.len() > 500);
        let content_lines: Vec<usize> = lines
            .iter()
            .filter(|l| !l.is_empty())
            .map(|l| l.len())
            .collect();
        let avg = content_lines.iter().sum::<usize>() as f64 / content_lines.len() as f64;
        assert!(
            (25.0..=160.0).contains(&avg),
            "average line length {avg} outside the plausible band"
        );
        assert!(text.contains(';') || text.contains('{'));
    }

    #[test]
    fn ragged_csv_line_lengths_vary_wildly() {
        let out = generate(Profile::RaggedCsv, 11, 1_048_576);
        let mut lengths: Vec<usize> = Vec::new();
        let mut current = 0usize;
        for &b in &out {
            if b == b'\n' {
                lengths.push(current);
                current = 0;
            } else if b != b'\r' {
                current += 1;
            }
        }
        let min = *lengths.iter().min().unwrap();
        let max = *lengths.iter().max().unwrap();
        assert!(max > min * 4, "min {min} max {max}: not ragged enough");
        assert!(max > 500, "expected some very long rows, max was {max}");
    }

    #[test]
    fn dirty_hex_has_nulls_escapes_and_surrogate_bytes() {
        let out = generate(Profile::DirtyHex, 3, 262_144);
        assert!(out.contains(&0x00));
        assert!(out.contains(&0x1b));
        // The orphaned-surrogate UTF-8 encoding ED A0 80 appears as a triple.
        assert!(out.windows(3).any(|w| w == [0xED, 0xA0, 0x80]));
    }

    #[test]
    fn unicode_stress_is_valid_utf8_with_rtl_marks() {
        let out = generate(Profile::UnicodeStress, 21, 131_072);
        // Whole output is staged from valid tokens + ASCII separators, so it
        // must decode — except possibly a token split at the sample edge.
        let text = String::from_utf8_lossy(&out[..out.len() - 16]);
        assert!(text.contains('\u{202e}'), "RTL override missing");
        assert!(text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
        assert!(text.contains('😀') || text.contains('🚀') || text.contains('🔥'));
    }

    #[test]
    fn needle_stamping_lands_at_exact_offsets() {
        // Synthetic stream: 'x' bytes with a newline every 50 bytes, fed to
        // the stamper in uneven blocks; needles every 100 bytes.
        let needle = b"TARGET";
        let every = 100u64;
        let total = 1000usize;
        let mut stream = vec![b'x'; total];
        for i in (49..total).step_by(50) {
            stream[i] = b'\n';
        }
        let mut hits = Vec::new();
        let mut carry = 0usize;
        let mut lines_before = 0u64;
        let mut pos = 0usize;
        let block_sizes = [37usize, 64, 3, 128, 256, 512];
        let mut bi = 0usize;
        while pos < total {
            let take = std::cmp::min(block_sizes[bi % block_sizes.len()], total - pos);
            bi += 1;
            let block = &mut stream[pos..pos + take];
            stamp_needles(
                block,
                pos as u64,
                lines_before,
                needle,
                every,
                &mut hits,
                &mut carry,
            );
            lines_before += count_newlines(block);
            pos += take;
        }
        assert_eq!(carry, 0);
        let expected_offsets: Vec<u64> = (0..10).map(|k| k * 100).collect();
        let got_offsets: Vec<u64> = hits.iter().map(|h| h.byte_offset).collect();
        assert_eq!(got_offsets, expected_offsets);
        for hit in &hits {
            let start = hit.byte_offset as usize;
            assert_eq!(
                &stream[start..start + needle.len()],
                needle,
                "needle missing at {}",
                hit.byte_offset
            );
            assert!(hit.line_number >= 1);
        }
        // Offset 0 is on line 1; offset 100 follows newlines at 49 and 99,
        // so it starts on line 3 — unless it overwrote the 99 newline,
        // which TARGET at 100 does not touch.
        assert_eq!(hits[0].line_number, 1);
        assert_eq!(hits[1].line_number, 3);
    }

    #[test]
    fn custom_line_len_range_is_enforced() {
        let mut cfg = test_cfg(Profile::DenseCode, 4);
        cfg.line_min = 80;
        cfg.line_max = 120;
        cfg.line_len_custom = true;
        let mut gen = Generator::new(&cfg);
        let mut out = vec![0u8; 262_144];
        gen.fill(&mut out);
        let text = String::from_utf8_lossy(&out);
        let lines: Vec<&str> = text.split("\r\n").collect();
        // Every complete line must sit inside 80..=120.
        for line in &lines[..lines.len() - 1] {
            assert!(
                (80..=120).contains(&line.len()),
                "line of {} chars outside 80..=120",
                line.len()
            );
        }
    }

    #[test]
    fn null_rate_zero_and_hundred() {
        let mut rng = Xoshiro256PlusPlus::from_seed(1);
        let mut block = vec![b'a'; 4096];
        apply_null_rate(&mut rng, &mut block, 0.0);
        assert!(block.iter().all(|&b| b == b'a'));
        apply_null_rate(&mut rng, &mut block, 100.0);
        assert!(block.iter().all(|&b| b == 0));
        let mut rng = Xoshiro256PlusPlus::from_seed(1);
        let mut block = vec![b'a'; 100_000];
        apply_null_rate(&mut rng, &mut block, 50.0);
        let nulls = block.iter().filter(|&&b| b == 0).count();
        assert!(
            (40_000..=60_000).contains(&nulls),
            "50% null rate produced {nulls} nulls"
        );
    }

    #[test]
    fn aligned_block_is_sector_aligned() {
        let block = AlignedBlock::new(BLOCK_SIZE).expect("allocation");
        assert_eq!(block.ptr as usize % SECTOR_SIZE as usize, 0);
        assert_eq!(block.as_slice().len(), BLOCK_SIZE);
    }
}
