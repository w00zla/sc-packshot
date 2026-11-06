// LD_PRELOAD shim for the Blender processes packshot starts on Linux: replaces the seed source of
// std::random_device (libstdc++'s std::random_device::_M_getval()) in that process, nothing else.
//
// Why: Blender 5.2 calls RandomNumberGenerator::from_random_seed() on every node tree update, unconditionally
// (source/blender/blenkernel/intern/node_tree_update.cc, ~line 2164), which constructs a std::random_device and
// reads it (source/blender/blenlib/intern/rand.cc, ~line 290). libstdc++ on x86-64 Linux reads it with RDSEED, a
// slow hardware source shared by all cores (~27 us per call here). render.py's node writes cause ~1860 calls per
// item, which capped the whole machine at ~13-14 items/s however many Blender processes ran; with this shim
// 6 Blender processes went from 12.9 to 22.1 items/s, images unchanged (2026-11-05).
//
// The values only seed a node tree's RNG, nothing here needs cryptographic randomness: splitmix64 over a shared
// counter (one atomic add per call, so thread-safe and lock-free: Blender calls it from many threads), the base
// seeded once from getrandom(). No libc: the .so must load into the official Blender build (glibc) whatever libc
// the machine building packshot has (the Linux release is static musl, it may be built on Windows), so it is
// no_std, calls getrandom through the raw syscall and links nothing (build.rs builds it with rustc and rust-lld).
// packshot_rdseed_shim_calls() tells a test how often the shim answered (ctypes in Blender's Python).
#![no_std]

use core::sync::atomic::{AtomicU64, Ordering};

/// The base of the sequence, 0 until the first call seeds it (always odd afterwards)
static SEED: AtomicU64 = AtomicU64::new(0);
/// Calls answered, the counter the sequence runs over
static CALLS: AtomicU64 = AtomicU64::new(0);

const SYS_GETRANDOM: usize = 318;
const EINTR: isize = 4;

/// getrandom(buf, 8, 0) by the raw x86-64 syscall; falls back to the time stamp counter and an address (ASLR)
fn fresh_seed() -> u64 {
    let mut v: u64 = 0;
    loop {
        let ret: isize;
        // SAFETY: getrandom writes at most 8 bytes into `v`; syscall clobbers rcx and r11 only
        unsafe {
            core::arch::asm!("syscall", inlateout("rax") SYS_GETRANDOM => ret, in("rdi") &raw mut v, in("rsi") 8usize, in("rdx") 0usize,
                lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        if ret == 8 {
            return v;
        }
        if ret != -EINTR {
            break;
        }
    }
    // SAFETY: rdtsc only reads the time stamp counter
    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
    tsc ^ (&raw const SEED as u64).rotate_left(32)
}

/// std::random_device::_M_getval(): the next splitmix64 value
#[unsafe(no_mangle)]
pub extern "C" fn _ZNSt13random_device9_M_getvalEv(_this: *mut u8) -> u32 {
    let mut seed = SEED.load(Ordering::Acquire);
    if seed == 0 {
        // two threads may both draw one: the first stored wins, the other takes it
        let mine = fresh_seed() | 1;
        seed = match SEED.compare_exchange(0, mine, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => mine,
            Err(stored) => stored,
        };
    }
    let n = CALLS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let mut z = seed.wrapping_add(n.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)) as u32
}

/// How often the shim answered (for the test in node.rs)
#[unsafe(no_mangle)]
pub extern "C" fn packshot_rdseed_shim_calls() -> u64 {
    CALLS.load(Ordering::Relaxed)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // SAFETY: nothing above panics; should it ever, the process stops with SIGILL
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}
