/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Runs tight guest loops and prints the interpreter's speed in MIPS.
//!
//! Run with `cargo run --release --example bench`.

use std::time::Instant;
use touchHLE_arm_interp::{Context, Interp};

/// loop: add r0, r0, #1; eor r1, r1, r0, lsl #3; ldr r2, [r3, #4];
/// str r2, [r3, #8]; add r1, r1, r2; cmp r0, r5; movlt r6, r0;
/// vadd.f32 s0, s0, s1; subs r4, r4, #1; bne loop; svc #0
const ARM_LOOP: &[u8] = &[
    0x01, 0x00, 0x80, 0xe2, 0x80, 0x11, 0x21, 0xe0, 0x04, 0x20, 0x93, 0xe5, 0x08, 0x20, 0x83, 0xe5,
    0x02, 0x10, 0x81, 0xe0, 0x05, 0x00, 0x50, 0xe1, 0x00, 0x60, 0xa0, 0xb1, 0x20, 0x0a, 0x30, 0xee,
    0x01, 0x40, 0x54, 0xe2, 0xf5, 0xff, 0xff, 0x1a, 0x00, 0x00, 0x00, 0xef,
];

/// The same in Thumb-2, with `it lt` before the `movlt`.
const THUMB_LOOP: &[u8] = &[
    0x01, 0x30, 0x81, 0xea, 0xc0, 0x01, 0x5a, 0x68, 0x9a, 0x60, 0x11, 0x44, 0xa8, 0x42, 0xb8, 0xbf,
    0x06, 0x46, 0x30, 0xee, 0x20, 0x0a, 0x01, 0x3c, 0xf2, 0xd1, 0x00, 0xdf,
];

fn bench(name: &str, code: &[u8], thumb: bool) {
    const ITERATIONS: u32 = 20_000_000;
    let mut mem = vec![0u8; 0x10000];
    mem[0x1000..0x1000 + code.len()].copy_from_slice(code);
    let mut interp = unsafe { Interp::new(mem.as_mut_ptr(), mem.len() as u64, 1) };
    let mut ctx = Context::new();
    ctx.regs[3] = 0x8000;
    ctx.regs[4] = ITERATIONS;
    ctx.regs[5] = ITERATIONS / 2;
    ctx.regs[15] = 0x1000;
    ctx.cpsr = 0x10 | if thumb { 0x20 } else { 0 };
    ctx.extregs[1] = 1.0f32.to_bits();
    interp.swap_context(&mut ctx);

    let start = Instant::now();
    let mut executed = 0;
    loop {
        // Similar to the time slices touchHLE uses.
        let mut ticks = 100_000;
        let res = interp.run_or_step(Some(&mut ticks));
        executed += 100_000 - ticks;
        match res {
            -1 => continue,
            0 => break,
            other => panic!("unexpected result {other}"),
        }
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{name}: {executed} instructions in {secs:.3}s: {:.0} MIPS",
        executed as f64 / secs / 1e6
    );
}

fn main() {
    bench("A32", ARM_LOOP, false);
    bench("T32", THUMB_LOOP, true);
}
