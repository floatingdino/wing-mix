//! push / snapshot_into must not allocate after construction (RT-adjacent analysis thread rule).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mix_analysis::signals::{interleave, pink};
use mix_analysis::{AnalyserConfig, StreamingAnalyser};
use mix_core::Window;

struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMED.with(Cell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ARMED.with(Cell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn run(channels: usize) {
    let sr = 48000;
    let n = sr as usize * 12;
    let l = pink(n, 0.1, 1);
    let r = pink(n, 0.1, 2);
    let x = if channels == 2 { interleave(&l, &r) } else { l };
    let mut an = StreamingAnalyser::new(AnalyserConfig::new(sr, channels)).unwrap();
    let windows = [
        Window::Momentary,
        Window::ShortTerm,
        Window::Long { tau_s: 10.0 },
        Window::Session,
    ];
    let mut outs: Vec<_> = windows
        .iter()
        .map(|w| an.new_features("main", *w))
        .collect();

    ARMED.with(|a| a.set(true));
    // 480-sample (10 ms) blocks like a capture callback; all four snapshots every 100 ms.
    for (i, block) in x.chunks(480 * channels).enumerate() {
        an.push(block);
        if i % 10 == 9 {
            for o in outs.iter_mut() {
                an.snapshot_into(o);
            }
        }
    }
    an.reset_session();
    an.push(&x[..48000 * channels]);
    ARMED.with(|a| a.set(false));
    assert_eq!(
        COUNT.with(Cell::get),
        0,
        "allocations in push/snapshot_into"
    );
    assert!(outs[3].loudness.integrated_lufs.is_some());
}

#[test]
fn push_and_snapshot_do_not_allocate_stereo() {
    run(2);
}

#[test]
fn push_and_snapshot_do_not_allocate_mono() {
    run(1);
}
