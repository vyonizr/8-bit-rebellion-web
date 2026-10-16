/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Portable ARMv7-A interpreter used in place of dynarmic where a JIT can't
//! run, such as WebAssembly.
//!
//! It implements user-mode A32 and T32 (including IT blocks), VFPv3, and the
//! subset of Advanced SIMD that the supported guests use, with the same
//! interface and observable behavior as the dynarmic wrapper in
//! `src/cpu/dynarmic_wrapper`.
//!
//! Instructions are decoded once into basic blocks of [insn::Insn], which are
//! cached by address until [Interp::invalidate_cache_range] is called.
#![allow(non_snake_case)]

mod arm;
mod exec;
mod insn;
mod thumb;
mod vfp;

use insn::{Insn, AL};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Saved CPU state. Same layout as `touchHLE_DynarmicContext`.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Context {
    pub regs: [u32; 16],
    /// S0-S31/D0-D31: D<n> is `extregs[2n]` (low) and `extregs[2n + 1]`.
    pub extregs: [u32; 64],
    pub cpsr: u32,
    pub fpscr: u32,
}

impl Default for Context {
    fn default() -> Self {
        Context {
            regs: [0; 16],
            extregs: [0; 64],
            cpsr: 0,
            fpscr: 0,
        }
    }
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }
}

/// CPSR bits held in separate fields of [Cpu] (NZCVQ, IT, GE, T).
const CPSR_SPLIT_MASK: u32 = 0xfe0f_fc20;

/// Architectural state and guest memory access.
pub(crate) struct Cpu {
    /// R0-R15. While an instruction executes, `r[15]` is the value reading
    /// the PC gives; otherwise it is the address of the next instruction.
    pub r: [u32; 16],
    pub ext: [u32; 64],
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    pub q: bool,
    pub ge: u32,
    pub thumb: bool,
    pub it: u8,
    /// CPSR bits not covered by [CPSR_SPLIT_MASK], such as the mode.
    pub cpsr_rest: u32,
    pub fpscr: u32,
    pub tpidruro: u32,
    /// Local exclusive monitor: the address of the last LDREX, if open.
    pub exclusive: Option<u32>,
    mem: *mut u8,
    /// Accesses below this address fail.
    null_end: u32,
    /// Number of accessible bytes from `null_end` on.
    span: u64,
}

impl Cpu {
    pub fn cpsr(&self) -> u32 {
        (self.n as u32) << 31
            | (self.z as u32) << 30
            | (self.c as u32) << 29
            | (self.v as u32) << 28
            | (self.q as u32) << 27
            | (self.it as u32 & 3) << 25
            | self.ge << 16
            | (self.it as u32 >> 2) << 10
            | (self.thumb as u32) << 5
            | self.cpsr_rest
    }

    pub fn set_cpsr(&mut self, v: u32) {
        self.n = v & (1 << 31) != 0;
        self.z = v & (1 << 30) != 0;
        self.c = v & (1 << 29) != 0;
        self.v = v & (1 << 28) != 0;
        self.q = v & (1 << 27) != 0;
        self.it = ((v >> 25) & 3 | ((v >> 10) & 0x3f) << 2) as u8;
        self.ge = (v >> 16) & 0xf;
        self.thumb = v & (1 << 5) != 0;
        self.cpsr_rest = v & !CPSR_SPLIT_MASK;
    }

    /// Host pointer for an access of `n` bytes, or [None] if any of it is
    /// outside accessible memory.
    #[inline(always)]
    fn host_ptr(&self, addr: u32, n: u32) -> Option<*mut u8> {
        let off = addr.wrapping_sub(self.null_end) as u64;
        if off + n as u64 > self.span {
            None
        } else {
            // Safety: checked against the mapped size above.
            Some(unsafe { self.mem.add(addr as usize) })
        }
    }

    #[inline(always)]
    pub fn read8(&self, addr: u32) -> Option<u8> {
        self.host_ptr(addr, 1).map(|p| unsafe { p.read() })
    }
    #[inline(always)]
    pub fn read16(&self, addr: u32) -> Option<u16> {
        self.host_ptr(addr, 2)
            .map(|p| u16::from_le(unsafe { p.cast::<u16>().read_unaligned() }))
    }
    #[inline(always)]
    pub fn read32(&self, addr: u32) -> Option<u32> {
        self.host_ptr(addr, 4)
            .map(|p| u32::from_le(unsafe { p.cast::<u32>().read_unaligned() }))
    }
    #[inline(always)]
    pub fn write8(&mut self, addr: u32, v: u8) -> Option<()> {
        self.host_ptr(addr, 1).map(|p| unsafe { p.write(v) })
    }
    #[inline(always)]
    pub fn write16(&mut self, addr: u32, v: u16) -> Option<()> {
        self.host_ptr(addr, 2)
            .map(|p| unsafe { p.cast::<u16>().write_unaligned(v.to_le()) })
    }
    #[inline(always)]
    pub fn write32(&mut self, addr: u32, v: u32) -> Option<()> {
        self.host_ptr(addr, 4)
            .map(|p| unsafe { p.cast::<u32>().write_unaligned(v.to_le()) })
    }
}

/// Hasher for block keys, which are already well-distributed apart from
/// their low bits.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_u64(&mut self, k: u64) {
        let h = k.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = h ^ (h >> 32);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy)]
struct Block {
    /// Index of the first instruction in [Cache::insns].
    start: u32,
    len: u32,
    /// The most recent successor block, to skip the hash lookup.
    next_key: u64,
    next_idx: u32,
}

const NO_BLOCK: u32 = u32::MAX;
const MAX_BLOCK_LEN: usize = 64;
/// The cache is flushed at the start of a run once it has grown past this
/// many instructions (20 bytes each), to bound memory use on wasm.
const MAX_CACHED_INSNS: usize = 1 << 20;

#[derive(Default)]
struct Cache {
    map: HashMap<u64, u32, BuildHasherDefault<KeyHasher>>,
    blocks: Vec<Block>,
    insns: Vec<Insn>,
}

pub struct Interp {
    cpu: Cpu,
    cache: Cache,
}

impl Interp {
    /// Create an interpreter accessing guest memory at `mem_base`, of which
    /// `mem_size` bytes are mapped. Accesses to the first `null_page_count`
    /// 4KiB pages or beyond `mem_size` are memory errors.
    ///
    /// # Safety
    ///
    /// `mem_base` must be valid for reads and writes of `mem_size` bytes
    /// whenever [Interp::run_or_step] is called.
    pub unsafe fn new(mem_base: *mut u8, mem_size: u64, null_page_count: usize) -> Self {
        let null_end = null_page_count as u64 * 0x1000;
        assert!(mem_size <= 1 << 32 && null_end <= mem_size);
        Interp {
            cpu: Cpu {
                r: [0; 16],
                ext: [0; 64],
                n: false,
                z: false,
                c: false,
                v: false,
                q: false,
                ge: 0,
                thumb: false,
                it: 0,
                cpsr_rest: 0,
                fpscr: 0,
                tpidruro: 0,
                exclusive: None,
                mem: mem_base,
                null_end: null_end as u32,
                span: mem_size - null_end,
            },
            cache: Cache::default(),
        }
    }

    pub fn regs(&self) -> &[u32; 16] {
        &self.cpu.r
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        &mut self.cpu.r
    }
    pub fn cpsr(&self) -> u32 {
        self.cpu.cpsr()
    }
    pub fn set_cpsr(&mut self, cpsr: u32) {
        self.cpu.set_cpsr(cpsr)
    }
    pub fn fpscr(&self) -> u32 {
        self.cpu.fpscr
    }
    pub fn extregs(&self) -> &[u32; 64] {
        &self.cpu.ext
    }
    pub fn extregs_mut(&mut self) -> &mut [u32; 64] {
        &mut self.cpu.ext
    }
    pub fn set_fpscr(&mut self, fpscr: u32) {
        self.cpu.fpscr = fpscr;
    }
    /// Set the value `mrc p15, 0, rX, c13, c0, 3` (TPIDRURO) returns.
    pub fn set_tpidruro(&mut self, value: u32) {
        self.cpu.tpidruro = value;
    }

    /// Swap the current state (registers etc) with the state stored in the
    /// context object.
    pub fn swap_context(&mut self, ctx: &mut Context) {
        let old = Context {
            regs: self.cpu.r,
            extregs: self.cpu.ext,
            cpsr: self.cpu.cpsr(),
            fpscr: self.cpu.fpscr,
        };
        self.cpu.r = ctx.regs;
        self.cpu.ext = ctx.extregs;
        self.cpu.set_cpsr(ctx.cpsr);
        self.cpu.fpscr = ctx.fpscr;
        self.cpu.exclusive = None;
        *ctx = old;
    }

    /// Discard decoded instructions for a range of addresses.
    pub fn invalidate_cache_range(&mut self, _start: u32, _size: u32) {
        // ponytail: flushes everything; this is only called when the dynamic
        // linker patches code. Track per-page blocks if it becomes frequent.
        self.flush_cache();
    }

    fn flush_cache(&mut self) {
        self.cache.map.clear();
        self.cache.blocks.clear();
        self.cache.insns.clear();
    }

    fn block_key(&self) -> u64 {
        self.cpu.r[15] as u64 | (self.cpu.thumb as u64) << 32 | (self.cpu.it as u64) << 33
    }

    /// Decodes a block starting at the current PC. Returns [NO_BLOCK] if the
    /// first instruction can't be fetched.
    #[cold]
    fn translate(&mut self, key: u64) -> u32 {
        let start = self.cache.insns.len();
        let thumb = self.cpu.thumb;
        let mut addr = self.cpu.r[15];
        let mut it = self.cpu.it;
        loop {
            let insn = if thumb {
                let Some(hw1) = self.cpu.read16(addr) else {
                    break;
                };
                let hw2 = if thumb::is_32bit(hw1) {
                    let Some(hw2) = self.cpu.read16(addr.wrapping_add(2)) else {
                        break;
                    };
                    hw2
                } else {
                    0
                };
                let (insn, next_it) = thumb::decode(hw1, hw2, addr, it);
                it = next_it;
                insn
            } else {
                let Some(w) = self.cpu.read32(addr) else {
                    break;
                };
                arm::decode(w, addr)
            };
            self.cache.insns.push(insn);
            addr = addr.wrapping_add(insn.len as u32);
            if insn.ends_block() || self.cache.insns.len() - start >= MAX_BLOCK_LEN {
                break;
            }
        }
        let len = self.cache.insns.len() - start;
        if len == 0 {
            return NO_BLOCK;
        }
        let idx = self.cache.blocks.len() as u32;
        self.cache.blocks.push(Block {
            start: start as u32,
            len: len as u32,
            next_key: u64::MAX,
            next_idx: NO_BLOCK,
        });
        self.cache.map.insert(key, idx);
        idx
    }

    fn lookup(&mut self, key: u64) -> u32 {
        match self.cache.map.get(&key) {
            Some(&idx) => idx,
            None => self.translate(key),
        }
    }

    /// Run until `ticks` (one per instruction) are used up, or execute one
    /// instruction if `ticks` is [None]. Returns -1 for normal completion,
    /// -2 for a memory error (PC is the faulting instruction), -3 for an
    /// undefined instruction, -4 for BKPT (PC is after the instruction), or
    /// the immediate of an SVC (PC is after the SVC).
    pub fn run_or_step(&mut self, ticks: Option<&mut u64>) -> i32 {
        match ticks {
            Some(ticks) => {
                if *ticks == 0 {
                    return -1;
                }
                self.run(ticks, usize::MAX)
            }
            None => self.run(&mut 1, 1),
        }
    }

    fn run(&mut self, ticks: &mut u64, max_insns: usize) -> i32 {
        // Not done in translate() because block indices must stay valid
        // during a run.
        if self.cache.insns.len() > MAX_CACHED_INSNS {
            self.flush_cache();
        }
        let mut prev = NO_BLOCK;
        loop {
            let key = self.block_key();
            let idx = if prev != NO_BLOCK && self.cache.blocks[prev as usize].next_key == key {
                self.cache.blocks[prev as usize].next_idx
            } else {
                let idx = self.lookup(key);
                if idx == NO_BLOCK {
                    return -2;
                }
                if prev != NO_BLOCK {
                    let p = &mut self.cache.blocks[prev as usize];
                    p.next_key = key;
                    p.next_idx = idx;
                }
                idx
            };
            let block = self.cache.blocks[idx as usize];
            let start = block.start as usize;
            let len = (block.len as usize).min(max_insns);
            let insns = &self.cache.insns[start..start + len];

            let cpu = &mut self.cpu;
            let mut status = exec::NEXT;
            let mut executed = 0;
            for insn in insns {
                executed += 1;
                if insn.cond != AL && !cpu.cond(insn.cond) {
                    continue;
                }
                cpu.r[15] = insn.pc;
                status = cpu.exec(insn);
                if status != exec::NEXT {
                    break;
                }
            }
            *ticks = ticks.saturating_sub(executed as u64);
            let last = &insns[executed - 1];

            let res = match status {
                exec::NEXT => {
                    cpu.r[15] = cpu.next_addr(last);
                    cpu.it = last.it_after();
                    None
                }
                exec::BRANCH => {
                    cpu.it = 0;
                    None
                }
                exec::MEMERR => {
                    cpu.r[15] = last.pc.wrapping_sub(if cpu.thumb { 4 } else { 8 });
                    cpu.it = last.it;
                    Some(-2)
                }
                _ => {
                    cpu.r[15] = cpu.next_addr(last);
                    cpu.it = last.it_after();
                    Some(match status {
                        exec::UNDEF => -3,
                        exec::BKPT => -4,
                        _ => last.imm as i32,
                    })
                }
            };
            if let Some(res) = res {
                return res;
            }
            if *ticks == 0 || max_insns != usize::MAX {
                return -1;
            }
            prev = idx;
        }
    }
}
