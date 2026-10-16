/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Tests of the interface semantics shared with the dynarmic wrapper.

use touchHLE_arm_interp::{Context, Interp};

const MOV_R0_1: u32 = 0xe3a00001;
const ADD_R0_1: u32 = 0xe2800001;
const SVC_42: u32 = 0xef00002a;
const B_SELF: u32 = 0xeafffffe;

fn setup(mem: &mut Vec<u8>, code: &[u32]) -> Interp {
    mem.resize(0x10000, 0);
    for (i, w) in code.iter().enumerate() {
        mem[0x1000 + i * 4..][..4].copy_from_slice(&w.to_le_bytes());
    }
    let mut interp = unsafe { Interp::new(mem.as_mut_ptr(), mem.len() as u64, 1) };
    interp.regs_mut()[15] = 0x1000;
    interp.set_cpsr(0x10);
    interp
}

#[test]
fn svc_and_ticks() {
    let mut mem = Vec::new();
    let mut cpu = setup(&mut mem, &[MOV_R0_1, ADD_R0_1, SVC_42]);
    let mut ticks = 100;
    assert_eq!(cpu.run_or_step(Some(&mut ticks)), 0x2a);
    assert_eq!(ticks, 97);
    assert_eq!(cpu.regs()[0], 2);
    // PC is after the SVC.
    assert_eq!(cpu.regs()[15], 0x100c);
}

#[test]
fn ticks_run_out() {
    let mut mem = Vec::new();
    let mut cpu = setup(&mut mem, &[B_SELF]);
    let mut ticks = 1000;
    assert_eq!(cpu.run_or_step(Some(&mut ticks)), -1);
    assert_eq!(ticks, 0);
    assert_eq!(cpu.regs()[15], 0x1000);
}

#[test]
fn step() {
    let mut mem = Vec::new();
    let mut cpu = setup(&mut mem, &[MOV_R0_1, ADD_R0_1]);
    assert_eq!(cpu.run_or_step(None), -1);
    assert_eq!((cpu.regs()[0], cpu.regs()[15]), (1, 0x1004));
    assert_eq!(cpu.run_or_step(None), -1);
    assert_eq!((cpu.regs()[0], cpu.regs()[15]), (2, 0x1008));
}

#[test]
fn memory_errors() {
    let mut mem = Vec::new();
    // ldr r0, [r1]
    let mut cpu = setup(&mut mem, &[MOV_R0_1, 0xe5910000]);
    // Null page.
    cpu.regs_mut()[1] = 0xffc;
    assert_eq!(cpu.run_or_step(Some(&mut 100)), -2);
    // PC identifies the faulting instruction.
    assert_eq!(cpu.regs()[15], 0x1004);
    assert_eq!(cpu.regs()[0], 1);
    // Beyond the mapped size, including straddling its end.
    for addr in [0x10000, 0xfffe, 0xffff_fffe] {
        cpu.regs_mut()[1] = addr;
        assert_eq!(cpu.run_or_step(Some(&mut 100)), -2);
    }
    cpu.regs_mut()[1] = 0xfffc;
    assert_eq!(cpu.run_or_step(Some(&mut 100)), -1);
    // Fetching from the null page.
    cpu.regs_mut()[15] = 0;
    assert_eq!(cpu.run_or_step(Some(&mut 100)), -2);
    assert_eq!(cpu.regs()[15], 0);
}

#[test]
fn undefined_and_bkpt() {
    let mut mem = Vec::new();
    // udf #0; bkpt #0
    let mut cpu = setup(&mut mem, &[0xe7f000f0, 0xe1200070]);
    assert_eq!(cpu.run_or_step(Some(&mut 100)), -3);
    // Like dynarmic, the PC is after the instruction.
    assert_eq!(cpu.regs()[15], 0x1004);
    assert_eq!(cpu.run_or_step(Some(&mut 100)), -4);
    assert_eq!(cpu.regs()[15], 0x1008);
}

#[test]
fn invalidate_cache() {
    let mut mem = Vec::new();
    let mut cpu = setup(&mut mem, &[MOV_R0_1, SVC_42]);
    assert_eq!(cpu.run_or_step(Some(&mut 100)), 0x2a);
    assert_eq!(cpu.regs()[0], 1);
    // mov r0, #2
    mem[0x1000..0x1004].copy_from_slice(&0xe3a00002u32.to_le_bytes());
    cpu.invalidate_cache_range(0x1000, 4);
    cpu.regs_mut()[15] = 0x1000;
    assert_eq!(cpu.run_or_step(Some(&mut 100)), 0x2a);
    assert_eq!(cpu.regs()[0], 2);
}

#[test]
fn thumb_interworking_and_tpidruro() {
    let mut mem = Vec::new();
    // ARM: blx r1 -> Thumb: mrc p15, 0, r0, c13, c0, 3; bx lr -> ARM: svc
    let mut cpu = setup(&mut mem, &[0xe12fff31, SVC_42]);
    mem[0x2000..0x2006].copy_from_slice(&[0x1d, 0xee, 0x70, 0x0f, 0x70, 0x47]);
    cpu.regs_mut()[1] = 0x2001;
    cpu.set_tpidruro(0xabcd);
    assert_eq!(cpu.run_or_step(Some(&mut 100)), 0x2a);
    assert_eq!(cpu.regs()[0], 0xabcd);
    assert_eq!(cpu.cpsr() & 0x20, 0);
}

#[test]
fn swap_context() {
    let mut mem = Vec::new();
    let mut cpu = setup(&mut mem, &[]);
    let mut ctx = Context::new();
    ctx.regs[3] = 7;
    ctx.extregs[63] = 9;
    ctx.cpsr = 0xf800_0030;
    ctx.fpscr = 0x0300_0000;
    cpu.swap_context(&mut ctx);
    assert_eq!(ctx.regs[15], 0x1000);
    assert_eq!(ctx.cpsr, 0x10);
    assert_eq!(cpu.regs()[3], 7);
    assert_eq!(cpu.cpsr(), 0xf800_0030);
    let mut back = Context::new();
    cpu.swap_context(&mut back);
    assert_eq!(back.extregs[63], 9);
    assert_eq!(back.fpscr, 0x0300_0000);
}
