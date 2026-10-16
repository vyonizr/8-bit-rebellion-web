/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Execution of decoded instructions.
//!
//! When an instruction executes, `r[15]` holds the value reading the PC
//! gives (the instruction address plus 8 or 4), so instructions reading
//! the PC need no special cases.

use crate::insn::*;
use crate::Cpu;

/// The instruction completed and execution continues sequentially.
pub const NEXT: u32 = 0;
/// The instruction wrote the PC.
pub const BRANCH: u32 = 1;
pub const MEMERR: u32 = 2;
pub const UNDEF: u32 = 3;
pub const BKPT: u32 = 4;
pub const SVC: u32 = 5;

/// Unwraps a memory access result or reports a memory error.
macro_rules! mem {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return MEMERR,
        }
    };
}

fn add_with_carry(x: u32, y: u32, carry_in: bool) -> (u32, bool, bool) {
    let (s1, c1) = x.overflowing_add(y);
    let (r, c2) = s1.overflowing_add(carry_in as u32);
    let v = ((x ^ r) & (y ^ r)) >> 31 != 0;
    (r, c1 | c2, v)
}

/// Shift with carry out. `amt` may be any value (register-specified shifts).
#[inline(always)]
fn shift_c(v: u32, ty: u8, amt: u32, c: bool) -> (u32, bool) {
    if amt == 0 && ty != shift::RRX {
        return (v, c);
    }
    match ty {
        shift::LSL => match amt {
            1..=31 => (v << amt, (v >> (32 - amt)) & 1 != 0),
            32 => (0, v & 1 != 0),
            _ => (0, false),
        },
        shift::LSR => match amt {
            1..=31 => (v >> amt, (v >> (amt - 1)) & 1 != 0),
            32 => (0, v >> 31 != 0),
            _ => (0, false),
        },
        shift::ASR => match amt {
            1..=31 => (((v as i32) >> amt) as u32, (v >> (amt - 1)) & 1 != 0),
            _ => (((v as i32) >> 31) as u32, v >> 31 != 0),
        },
        shift::ROR => {
            let r = v.rotate_right(amt & 31);
            (r, r >> 31 != 0)
        }
        _ => ((c as u32) << 31 | v >> 1, v & 1 != 0),
    }
}

fn sext8(v: u32) -> u32 {
    v as u8 as i8 as i32 as u32
}
fn sext16(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}
fn lo16(v: u32) -> i64 {
    v as u16 as i16 as i64
}
fn hi16(v: u32) -> i64 {
    (v >> 16) as u16 as i16 as i64
}

/// Signed saturation to `n` bits (1..=32): returns (value, saturated).
fn ssat(v: i64, n: u32) -> (u32, bool) {
    let max = (1i64 << (n - 1)) - 1;
    let min = -(1i64 << (n - 1));
    if v > max {
        (max as u32, true)
    } else if v < min {
        (min as u32, true)
    } else {
        (v as u32, false)
    }
}

/// Unsigned saturation to `n` bits (0..=31).
fn usat(v: i64, n: u32) -> (u32, bool) {
    let max = (1i64 << n) - 1;
    if v > max {
        (max as u32, true)
    } else if v < 0 {
        (0, true)
    } else {
        (v as u32, false)
    }
}

impl Cpu {
    #[inline(always)]
    pub(crate) fn reg(&self, r: u8) -> u32 {
        self.r[(r & 15) as usize]
    }
    #[inline(always)]
    fn set(&mut self, r: u8, v: u32) {
        self.r[(r & 15) as usize] = v;
    }

    #[inline(always)]
    pub(crate) fn cond(&self, cond: u8) -> bool {
        match cond {
            0 => self.z,
            1 => !self.z,
            2 => self.c,
            3 => !self.c,
            4 => self.n,
            5 => !self.n,
            6 => self.v,
            7 => !self.v,
            8 => self.c && !self.z,
            9 => !self.c || self.z,
            10 => self.n == self.v,
            11 => self.n != self.v,
            12 => !self.z && self.n == self.v,
            13 => self.z || self.n != self.v,
            _ => true,
        }
    }

    /// Address of the instruction following `i`.
    pub(crate) fn next_addr(&self, i: &Insn) -> u32 {
        i.pc.wrapping_sub(if self.thumb { 4 } else { 8 })
            .wrapping_add(i.len as u32)
    }

    /// BXWritePC (also LoadWritePC, and ALUWritePC in ARM state).
    fn bx_write_pc(&mut self, v: u32) {
        if v & 1 != 0 {
            self.thumb = true;
            self.r[15] = v & !1;
        } else {
            self.thumb = false;
            self.r[15] = v & !3;
        }
    }

    fn alu_write_pc(&mut self, v: u32) {
        if self.thumb {
            self.r[15] = v & !1;
        } else {
            self.bx_write_pc(v);
        }
    }

    fn link(&mut self, i: &Insn) {
        self.r[14] = self.next_addr(i) | self.thumb as u32;
    }

    #[inline(always)]
    fn alu(&mut self, i: &Insn, a: u32, b: u32, shift_carry: bool) -> u32 {
        let (r, cv) = match i.op {
            alu::AND | alu::TST => (a & b, None),
            alu::EOR | alu::TEQ => (a ^ b, None),
            alu::ORR => (a | b, None),
            alu::MOV => (b, None),
            alu::BIC => (a & !b, None),
            alu::MVN => (!b, None),
            alu::ORN => (a | !b, None),
            op => {
                let (r, c, v) = match op {
                    alu::SUB | alu::CMP => add_with_carry(a, !b, true),
                    alu::RSB => add_with_carry(b, !a, true),
                    alu::ADD | alu::CMN => add_with_carry(a, b, false),
                    alu::ADC => add_with_carry(a, b, self.c),
                    alu::SBC => add_with_carry(a, !b, self.c),
                    alu::RSC => add_with_carry(b, !a, self.c),
                    _ => unreachable!(),
                };
                (r, Some((c, v)))
            }
        };
        if i.flags & F_S != 0 {
            self.n = r >> 31 != 0;
            self.z = r == 0;
            match cv {
                Some((c, v)) => {
                    self.c = c;
                    self.v = v;
                }
                None => self.c = shift_carry,
            }
        }
        if (alu::TST..=alu::CMN).contains(&i.op) {
            NEXT
        } else if i.rd == 15 {
            self.alu_write_pc(r);
            BRANCH
        } else {
            self.set(i.rd, r);
            NEXT
        }
    }

    /// Address computation for single loads/stores: (address, writeback
    /// value).
    #[inline(always)]
    fn ls_addr(&self, i: &Insn) -> (u32, u32) {
        let base = if i.rn == 15 {
            self.r[15] & !3
        } else {
            self.reg(i.rn)
        };
        let off = if i.flags & F_REG != 0 {
            shift_c(self.reg(i.rm), i.sh, i.amt as u32, self.c).0
        } else {
            i.imm
        };
        let oa = if i.flags & F_U != 0 {
            base.wrapping_add(off)
        } else {
            base.wrapping_sub(off)
        };
        (if i.flags & F_P != 0 { oa } else { base }, oa)
    }

    #[inline(always)]
    fn ls_writeback(&mut self, i: &Insn, wb: u32) {
        if i.flags & (F_P | F_W) != F_P && i.rn != 15 {
            self.set(i.rn, wb);
        }
    }

    /// Start address and writeback value for load/store multiple.
    fn lsm_range(&self, mode: u8, base: u32, words: u32) -> (u32, u32) {
        let size = words * 4;
        match mode {
            lsm::IA => (base, base.wrapping_add(size)),
            lsm::IB => (base.wrapping_add(4), base.wrapping_add(size)),
            lsm::DA => (
                base.wrapping_sub(size).wrapping_add(4),
                base.wrapping_sub(size),
            ),
            _ => (base.wrapping_sub(size), base.wrapping_sub(size)),
        }
    }

    /// Executes one instruction whose condition passed. Returns one of the
    /// status constants in this module.
    #[inline(always)]
    pub(crate) fn exec(&mut self, i: &Insn) -> u32 {
        match i.kind {
            Kind::DpImm => {
                let carry = match i.sh {
                    0 => false,
                    1 => true,
                    _ => self.c,
                };
                self.alu(i, self.reg(i.rn), i.imm, carry)
            }
            Kind::DpRegImm => {
                let (b, c) = shift_c(self.reg(i.rm), i.sh, i.amt as u32, self.c);
                self.alu(i, self.reg(i.rn), b, c)
            }
            Kind::DpRegReg => {
                let amt = self.reg(i.ra) & 0xff;
                let (b, c) = shift_c(self.reg(i.rm), i.sh, amt, self.c);
                self.alu(i, self.reg(i.rn), b, c)
            }
            Kind::Ldr => {
                let (a, wb) = self.ls_addr(i);
                let v = mem!(self.read32(a));
                self.ls_writeback(i, wb);
                if i.rd == 15 {
                    self.bx_write_pc(v);
                    return BRANCH;
                }
                self.set(i.rd, v);
                NEXT
            }
            Kind::Str => {
                let (a, wb) = self.ls_addr(i);
                mem!(self.write32(a, self.reg(i.rd)));
                self.ls_writeback(i, wb);
                NEXT
            }
            Kind::Ldrb | Kind::Ldrh | Kind::Ldrsb | Kind::Ldrsh => {
                let (a, wb) = self.ls_addr(i);
                let v = match i.kind {
                    Kind::Ldrb => mem!(self.read8(a)) as u32,
                    Kind::Ldrh => mem!(self.read16(a)) as u32,
                    Kind::Ldrsb => sext8(mem!(self.read8(a)) as u32),
                    _ => sext16(mem!(self.read16(a)) as u32),
                };
                self.ls_writeback(i, wb);
                self.set(i.rd, v);
                NEXT
            }
            Kind::Strb => {
                let (a, wb) = self.ls_addr(i);
                mem!(self.write8(a, self.reg(i.rd) as u8));
                self.ls_writeback(i, wb);
                NEXT
            }
            Kind::Strh => {
                let (a, wb) = self.ls_addr(i);
                mem!(self.write16(a, self.reg(i.rd) as u16));
                self.ls_writeback(i, wb);
                NEXT
            }
            Kind::Ldrd => {
                let (a, wb) = self.ls_addr(i);
                let lo = mem!(self.read32(a));
                let hi = mem!(self.read32(a.wrapping_add(4)));
                self.ls_writeback(i, wb);
                self.set(i.rd, lo);
                self.set(i.ra, hi);
                NEXT
            }
            Kind::Strd => {
                let (a, wb) = self.ls_addr(i);
                mem!(self.write32(a, self.reg(i.rd)));
                mem!(self.write32(a.wrapping_add(4), self.reg(i.ra)));
                self.ls_writeback(i, wb);
                NEXT
            }
            Kind::Ldm => {
                let list = i.imm;
                let (mut a, wb) = self.lsm_range(i.op, self.reg(i.rn), list.count_ones());
                let mut vals = [0u32; 16];
                for (r, val) in vals.iter_mut().enumerate() {
                    if list & (1 << r) != 0 {
                        *val = mem!(self.read32(a));
                        a = a.wrapping_add(4);
                    }
                }
                // If the base is in the list, the loaded value wins.
                if i.flags & F_W != 0 && list & (1 << i.rn) == 0 {
                    self.set(i.rn, wb);
                }
                for (r, &val) in vals.iter().enumerate().take(15) {
                    if list & (1 << r) != 0 {
                        self.r[r] = val;
                    }
                }
                if list & 0x8000 != 0 {
                    self.bx_write_pc(vals[15]);
                    return BRANCH;
                }
                NEXT
            }
            Kind::Stm => {
                let list = i.imm;
                let (mut a, wb) = self.lsm_range(i.op, self.reg(i.rn), list.count_ones());
                for r in 0..16 {
                    if list & (1 << r) != 0 {
                        mem!(self.write32(a, self.r[r]));
                        a = a.wrapping_add(4);
                    }
                }
                if i.flags & F_W != 0 {
                    self.set(i.rn, wb);
                }
                NEXT
            }
            Kind::B => {
                self.r[15] = i.imm;
                BRANCH
            }
            Kind::Bl => {
                self.link(i);
                self.r[15] = i.imm;
                BRANCH
            }
            Kind::BlxImm => {
                self.link(i);
                self.thumb = !self.thumb;
                self.r[15] = i.imm;
                BRANCH
            }
            Kind::Bx => {
                self.bx_write_pc(self.reg(i.rm));
                BRANCH
            }
            Kind::BlxReg => {
                let target = self.reg(i.rm);
                self.link(i);
                self.bx_write_pc(target);
                BRANCH
            }
            Kind::Cbz => {
                if (self.reg(i.rn) == 0) != (i.op != 0) {
                    self.r[15] = i.imm;
                    BRANCH
                } else {
                    NEXT
                }
            }
            Kind::Tbb => {
                let base = self.reg(i.rn);
                let idx = self.reg(i.rm);
                let off = if i.op == 0 {
                    mem!(self.read8(base.wrapping_add(idx))) as u32
                } else {
                    mem!(self.read16(base.wrapping_add(idx << 1))) as u32
                };
                self.r[15] = i.pc.wrapping_add(off * 2);
                BRANCH
            }
            Kind::Movw => {
                self.set(i.rd, i.imm);
                NEXT
            }
            Kind::Movt => {
                let v = (self.reg(i.rd) & 0xffff) | i.imm << 16;
                self.set(i.rd, v);
                NEXT
            }
            Kind::Nop => NEXT,
            Kind::Svc => SVC,
            Kind::Bkpt => BKPT,
            Kind::Undef => UNDEF,
            Kind::Vfp => {
                self.exec_vfp(i);
                NEXT
            }
            Kind::NeonF32 => {
                self.exec_neon_f32(i);
                NEXT
            }
            Kind::Vldr | Kind::Vstr => {
                let base = if i.rn == 15 {
                    self.r[15] & !3
                } else {
                    self.reg(i.rn)
                };
                let a = if i.flags & F_U != 0 {
                    base.wrapping_add(i.imm)
                } else {
                    base.wrapping_sub(i.imm)
                };
                let (idx, words) = if i.flags & F_DBL != 0 {
                    (i.rd as usize * 2, 2)
                } else {
                    (i.rd as usize, 1)
                };
                self.ext_transfer(i.kind == Kind::Vldr, a, idx, words)
            }
            Kind::Vldm | Kind::Vstm => {
                let base = self.reg(i.rn);
                let (idx, words) = if i.flags & F_DBL != 0 {
                    (i.rd as usize * 2, i.imm * 2)
                } else {
                    (i.rd as usize, i.imm)
                };
                let (a, wb) = self.lsm_range(i.op, base, words);
                let res = self.ext_transfer(i.kind == Kind::Vldm, a, idx, words);
                if res == NEXT && i.flags & F_W != 0 {
                    self.set(i.rn, wb);
                }
                res
            }
            _ => self.exec_rare(i),
        }
    }

    fn ext_transfer(&mut self, load: bool, mut a: u32, idx: usize, words: u32) -> u32 {
        for k in 0..words as usize {
            let e = (idx + k) & 63;
            if load {
                self.ext[e] = mem!(self.read32(a));
            } else {
                mem!(self.write32(a, self.ext[e]));
            }
            a = a.wrapping_add(4);
        }
        NEXT
    }

    /// Less frequent instructions, kept out of line.
    #[inline(never)]
    fn exec_rare(&mut self, i: &Insn) -> u32 {
        match i.kind {
            Kind::Mul => {
                let p = self.reg(i.rn).wrapping_mul(self.reg(i.rm));
                let r = match i.op {
                    0 => p,
                    1 => p.wrapping_add(self.reg(i.ra)),
                    _ => self.reg(i.ra).wrapping_sub(p),
                };
                if i.flags & F_S != 0 {
                    self.n = r >> 31 != 0;
                    self.z = r == 0;
                }
                self.set(i.rd, r);
            }
            Kind::MulLong => {
                let (n, m) = (self.reg(i.rn), self.reg(i.rm));
                let acc = (self.reg(i.ra) as u64) << 32 | self.reg(i.rd) as u64;
                let r = match i.op {
                    mul_long::UMULL => n as u64 * m as u64,
                    mul_long::UMLAL => (n as u64 * m as u64).wrapping_add(acc),
                    mul_long::SMULL => (n as i32 as i64 * m as i32 as i64) as u64,
                    mul_long::SMLAL => {
                        ((n as i32 as i64 * m as i32 as i64) as u64).wrapping_add(acc)
                    }
                    _ => n as u64 * m as u64 + self.reg(i.rd) as u64 + self.reg(i.ra) as u64,
                };
                if i.flags & F_S != 0 {
                    self.n = r >> 63 != 0;
                    self.z = r == 0;
                }
                self.set(i.rd, r as u32);
                self.set(i.ra, (r >> 32) as u32);
            }
            Kind::Smmul => {
                let p = self.reg(i.rn) as i32 as i64 * self.reg(i.rm) as i32 as i64;
                let acc = (self.reg(i.ra) as i64) << 32;
                let mut r = match i.op {
                    0 => p,
                    1 => acc.wrapping_add(p),
                    _ => acc.wrapping_sub(p),
                };
                if i.amt != 0 {
                    r = r.wrapping_add(0x8000_0000);
                }
                self.set(i.rd, (r >> 32) as u32);
            }
            Kind::SmulXy => {
                let (n, m) = (self.reg(i.rn), self.reg(i.rm));
                let x = if i.sh & 1 != 0 { hi16(n) } else { lo16(n) };
                let y = if i.sh & 2 != 0 { hi16(m) } else { lo16(m) };
                match i.op {
                    0 => self.set(i.rd, (x * y) as u32),
                    1 | 3 => {
                        let p = if i.op == 1 {
                            x * y
                        } else {
                            (n as i32 as i64 * y) >> 16
                        };
                        let r = p + self.reg(i.ra) as i32 as i64;
                        if r != r as i32 as i64 {
                            self.q = true;
                        }
                        self.set(i.rd, r as u32);
                    }
                    2 => self.set(i.rd, ((n as i32 as i64 * y) >> 16) as u32),
                    _ => {
                        let acc = (self.reg(i.ra) as u64) << 32 | self.reg(i.rd) as u64;
                        let r = acc.wrapping_add((x * y) as u64);
                        self.set(i.rd, r as u32);
                        self.set(i.ra, (r >> 32) as u32);
                    }
                }
            }
            Kind::SmDual => {
                let n = self.reg(i.rn);
                let m = self.reg(i.rm);
                let m = if i.amt != 0 { m.rotate_right(16) } else { m };
                let p1 = lo16(n) * lo16(m);
                let p2 = hi16(n) * hi16(m);
                let sum = if i.op & 1 == 0 { p1 + p2 } else { p1 - p2 };
                if i.op < 2 {
                    let r = if i.ra == 15 {
                        sum
                    } else {
                        sum + self.reg(i.ra) as i32 as i64
                    };
                    if r != r as i32 as i64 {
                        self.q = true;
                    }
                    self.set(i.rd, r as u32);
                } else {
                    let acc = (self.reg(i.ra) as u64) << 32 | self.reg(i.rd) as u64;
                    let r = acc.wrapping_add(sum as u64);
                    self.set(i.rd, r as u32);
                    self.set(i.ra, (r >> 32) as u32);
                }
            }
            Kind::Qarith => {
                let m = self.reg(i.rm) as i32 as i64;
                let mut n = self.reg(i.rn) as i32 as i64;
                if i.op >= 2 {
                    let (d, sat) = ssat(n * 2, 32);
                    self.q |= sat;
                    n = d as i32 as i64;
                }
                let (r, sat) = ssat(if i.op & 1 == 0 { m + n } else { m - n }, 32);
                self.q |= sat;
                self.set(i.rd, r);
            }
            Kind::Ssat | Kind::Usat => {
                let v = shift_c(self.reg(i.rn), i.sh, i.amt as u32, self.c).0 as i32 as i64;
                let (r, sat) = if i.kind == Kind::Ssat {
                    ssat(v, i.imm)
                } else {
                    usat(v, i.imm)
                };
                self.q |= sat;
                self.set(i.rd, r);
            }
            Kind::Ssat16 | Kind::Usat16 => {
                let v = self.reg(i.rn);
                let mut r = 0;
                for (k, h) in [lo16(v), hi16(v)].into_iter().enumerate() {
                    let (x, sat) = if i.kind == Kind::Ssat16 {
                        ssat(h, i.imm)
                    } else {
                        usat(h, i.imm)
                    };
                    self.q |= sat;
                    r |= (x & 0xffff) << (16 * k);
                }
                self.set(i.rd, r);
            }
            Kind::Extend => {
                let v = self.reg(i.rm).rotate_right(i.amt as u32);
                let add = if i.rn == 15 { 0 } else { self.reg(i.rn) };
                let r = match i.op {
                    ext::SXTB => sext8(v).wrapping_add(add),
                    ext::SXTH => sext16(v).wrapping_add(add),
                    ext::UXTB => (v & 0xff).wrapping_add(add),
                    ext::UXTH => (v & 0xffff).wrapping_add(add),
                    op => {
                        let f = |b: u32| {
                            if op == ext::SXTB16 {
                                sext8(b)
                            } else {
                                b & 0xff
                            }
                        };
                        let lo = f(v).wrapping_add(add) & 0xffff;
                        let hi = f(v >> 16).wrapping_add(add >> 16) & 0xffff;
                        hi << 16 | lo
                    }
                };
                self.set(i.rd, r);
            }
            Kind::Rev => {
                let v = self.reg(i.rm);
                let r = match i.op {
                    0 => v.swap_bytes(),
                    1 => (v >> 8) & 0x00ff_00ff | (v << 8) & 0xff00_ff00,
                    2 => sext16((v & 0xff) << 8 | (v >> 8) & 0xff),
                    _ => v.reverse_bits(),
                };
                self.set(i.rd, r);
            }
            Kind::Clz => self.set(i.rd, self.reg(i.rm).leading_zeros()),
            Kind::Bfc | Kind::Bfi => {
                let (lsb, msb) = (i.amt as u32, i.imm);
                if msb >= lsb {
                    let mask = (((1u64 << (msb - lsb + 1)) - 1) << lsb) as u32;
                    let ins = if i.kind == Kind::Bfi {
                        self.reg(i.rn) << lsb
                    } else {
                        0
                    };
                    self.set(i.rd, (self.reg(i.rd) & !mask) | (ins & mask));
                }
            }
            Kind::Ubfx | Kind::Sbfx => {
                let (lsb, wm1) = (i.amt as u32, i.imm);
                let msb = lsb + wm1;
                if msb <= 31 {
                    let v = self.reg(i.rn) << (31 - msb);
                    let r = if i.kind == Kind::Ubfx {
                        v >> (31 - wm1)
                    } else {
                        ((v as i32) >> (31 - wm1)) as u32
                    };
                    self.set(i.rd, r);
                }
            }
            Kind::Sel => {
                let (n, m) = (self.reg(i.rn), self.reg(i.rm));
                let mut mask = 0;
                for k in 0..4 {
                    if self.ge & (1 << k) != 0 {
                        mask |= 0xff << (8 * k);
                    }
                }
                self.set(i.rd, (n & mask) | (m & !mask));
            }
            Kind::ParAddSub => self.par_add_sub(i),
            Kind::Pkh => {
                let (n, m) = (self.reg(i.rn), self.reg(i.rm));
                let r = if i.sh == 0 {
                    (n & 0xffff) | (m << i.amt) & 0xffff_0000
                } else {
                    let amt = if i.amt == 0 { 31 } else { i.amt };
                    (n & 0xffff_0000) | (((m as i32) >> amt) as u32 & 0xffff)
                };
                self.set(i.rd, r);
            }
            Kind::Usad8 => {
                let (n, m) = (self.reg(i.rn), self.reg(i.rm));
                let mut sum = if i.ra == 15 { 0 } else { self.reg(i.ra) };
                for k in 0..4 {
                    let a = (n >> (8 * k)) & 0xff;
                    let b = (m >> (8 * k)) & 0xff;
                    sum = sum.wrapping_add(a.abs_diff(b));
                }
                self.set(i.rd, sum);
            }
            Kind::Ldrex => {
                let a = self.reg(i.rn).wrapping_add(i.imm);
                match i.op {
                    1 => {
                        let v = mem!(self.read8(a));
                        self.set(i.rd, v as u32);
                    }
                    2 => {
                        let v = mem!(self.read16(a));
                        self.set(i.rd, v as u32);
                    }
                    4 => {
                        let v = mem!(self.read32(a));
                        self.set(i.rd, v);
                    }
                    _ => {
                        let lo = mem!(self.read32(a));
                        let hi = mem!(self.read32(a.wrapping_add(4)));
                        self.set(i.rd, lo);
                        self.set(i.ra, hi);
                    }
                }
                self.exclusive = Some(a);
            }
            Kind::Strex => {
                let a = self.reg(i.rn).wrapping_add(i.imm);
                let status = if self.exclusive == Some(a) {
                    let v = self.reg(i.rm);
                    match i.op {
                        1 => mem!(self.write8(a, v as u8)),
                        2 => mem!(self.write16(a, v as u16)),
                        4 => mem!(self.write32(a, v)),
                        _ => {
                            mem!(self.write32(a, v));
                            mem!(self.write32(a.wrapping_add(4), self.reg(i.ra)));
                        }
                    }
                    0
                } else {
                    1
                };
                self.exclusive = None;
                self.set(i.rd, status);
            }
            Kind::Clrex => self.exclusive = None,
            Kind::Mrs => self.set(i.rd, self.cpsr() & !0x0600_fc20),
            Kind::Msr => {
                let v = if i.flags & F_IMM != 0 {
                    i.imm
                } else {
                    self.reg(i.rn)
                };
                if i.op & 2 != 0 {
                    self.n = v & (1 << 31) != 0;
                    self.z = v & (1 << 30) != 0;
                    self.c = v & (1 << 29) != 0;
                    self.v = v & (1 << 28) != 0;
                    self.q = v & (1 << 27) != 0;
                }
                if i.op & 1 != 0 {
                    self.ge = (v >> 16) & 0xf;
                }
            }
            Kind::MrcTpidruro => self.set(i.rd, self.tpidruro),
            Kind::VmovCoreS => {
                if i.op != 0 {
                    self.set(i.rd, self.ext[(i.rn & 31) as usize]);
                } else {
                    self.ext[(i.rn & 31) as usize] = self.reg(i.rd);
                }
            }
            Kind::VmovCore2 => {
                let e = if i.flags & F_DBL != 0 {
                    i.rm as usize * 2
                } else {
                    i.rm as usize
                } & 63;
                if i.op != 0 {
                    self.set(i.rd, self.ext[e]);
                    self.set(i.ra, self.ext[(e + 1) & 63]);
                } else {
                    self.ext[e] = self.reg(i.rd);
                    self.ext[(e + 1) & 63] = self.reg(i.ra);
                }
            }
            Kind::VmovScalar => {
                let e = (i.rn as usize * 2 + i.amt as usize) & 63;
                if i.op != 0 {
                    self.set(i.rd, self.ext[e]);
                } else {
                    self.ext[e] = self.reg(i.rd);
                }
            }
            Kind::Vmrs => {
                if i.rd == 15 {
                    let f = self.fpscr;
                    self.n = f & (1 << 31) != 0;
                    self.z = f & (1 << 30) != 0;
                    self.c = f & (1 << 29) != 0;
                    self.v = f & (1 << 28) != 0;
                } else {
                    self.set(i.rd, self.fpscr);
                }
            }
            Kind::Vmsr => self.fpscr = self.reg(i.rn) & crate::vfp::FPSCR_MASK,
            _ => unreachable!("{:?}", i.kind),
        }
        NEXT
    }

    fn par_add_sub(&mut self, i: &Insn) {
        let (a, b) = (self.reg(i.rn), self.reg(i.rm));
        let unsigned = i.op & 4 != 0;
        let (width, lanes) = if i.sh >= par::ADD8 { (8, 4) } else { (16, 2) };
        let mask = (1u32 << width) - 1;
        let lane = |v: u32, k: u32| -> i64 {
            let x = (v >> (width * k)) & mask;
            if unsigned {
                x as i64
            } else if width == 8 {
                x as u8 as i8 as i64
            } else {
                x as u16 as i16 as i64
            }
        };
        let mut r = 0;
        let mut ge = 0;
        for k in 0..lanes {
            // (x, y, add) for this lane.
            let (x, y, add) = match i.sh {
                par::ADD16 | par::ADD8 => (lane(a, k), lane(b, k), true),
                par::SUB16 | par::SUB8 => (lane(a, k), lane(b, k), false),
                par::ASX => (lane(a, k), lane(b, 1 - k), k == 1),
                _ => (lane(a, k), lane(b, 1 - k), k == 0),
            };
            let v = if add { x + y } else { x - y };
            let res = match i.op & 3 {
                0 => {
                    let set = if unsigned && add {
                        v >= 1 << width
                    } else {
                        v >= 0
                    };
                    if set {
                        ge |= if width == 8 { 1 << k } else { 3 << (2 * k) };
                    }
                    v as u32
                }
                1 => {
                    if unsigned {
                        usat(v, width).0
                    } else {
                        ssat(v, width).0
                    }
                }
                _ => (v >> 1) as u32,
            };
            r |= (res & mask) << (width * k);
        }
        if i.op & 3 == 0 {
            self.ge = ge;
        }
        self.set(i.rd, r);
    }
}
