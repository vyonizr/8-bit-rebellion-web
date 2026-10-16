/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! A32 (ARM instruction set) decoder.
//!
//! Section and table references are to the ARMv7-A/R Architecture Reference
//! Manual (DDI 0406C), chapter A5.

use crate::insn::*;
use crate::vfp;

fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1 << (hi - lo + 1)) - 1)
}
fn bit(w: u32, b: u32) -> bool {
    (w >> b) & 1 != 0
}

/// DecodeImmShift for immediate shifts: returns (type, amount).
pub fn imm_shift(ty: u32, imm5: u32) -> (u8, u8) {
    match (ty, imm5) {
        (0, n) => (shift::LSL, n as u8),
        (1, 0) => (shift::LSR, 32),
        (2, 0) => (shift::ASR, 32),
        (3, 0) => (shift::RRX, 1),
        (t, n) => (t as u8, n as u8),
    }
}

/// Load/store P, U and W bits as flags.
fn puw(w: u32) -> u8 {
    let mut f = 0;
    if bit(w, 24) {
        f |= F_P;
    }
    if bit(w, 23) {
        f |= F_U;
    }
    if bit(w, 21) {
        f |= F_W;
    }
    f
}

pub fn decode(w: u32, addr: u32) -> Insn {
    let mut i = decode_inner(w);
    i.pc = addr.wrapping_add(8);
    i.len = 4;
    if i.cond == AL && w >> 28 != 0xf {
        i.cond = (w >> 28) as u8;
    }
    // Branch targets are encoded relative to the PC.
    if matches!(i.kind, Kind::B | Kind::Bl | Kind::BlxImm) {
        i.imm = i.imm.wrapping_add(i.pc);
    }
    i
}

fn undef() -> Insn {
    Insn::new(Kind::Undef)
}

pub(crate) fn decode_inner(w: u32) -> Insn {
    let rn = bits(w, 19, 16) as u8;
    let rd = bits(w, 15, 12) as u8;
    let rs = bits(w, 11, 8) as u8;
    let rm = bits(w, 3, 0) as u8;

    if w >> 28 == 0xf {
        return decode_unconditional(w);
    }

    match bits(w, 27, 25) {
        0b000 | 0b001 => {
            let imm = bit(w, 25);
            if !imm && bit(w, 4) && bit(w, 7) {
                return decode_mul_extra(w);
            }
            if bits(w, 24, 23) == 0b10 && !bit(w, 20) {
                if imm {
                    return match bits(w, 22, 21) {
                        0b00 => Insn {
                            rd,
                            imm: (rn as u32) << 12 | bits(w, 11, 0),
                            ..Insn::new(Kind::Movw)
                        },
                        0b10 => Insn {
                            rd,
                            imm: (rn as u32) << 12 | bits(w, 11, 0),
                            ..Insn::new(Kind::Movt)
                        },
                        0b01 => {
                            let mask = bits(w, 19, 16);
                            if mask == 0 {
                                // Hints: NOP, YIELD, WFE, WFI, SEV, DBG.
                                return Insn::new(Kind::Nop);
                            }
                            msr(mask, F_IMM, 0, expand_imm(w).0)
                        }
                        _ => undef(),
                    };
                }
                return decode_misc(w);
            }
            // Data processing.
            let op = bits(w, 24, 21) as u8;
            let s = bit(w, 20);
            // S=1 with Rd=PC is an exception return, not usable in user mode.
            if s && rd == 15 && !(alu::TST..=alu::CMN).contains(&op) {
                return undef();
            }
            let flags = if s { F_S } else { 0 };
            if imm {
                let (imm, carry) = expand_imm(w);
                Insn {
                    op,
                    flags,
                    rd,
                    rn,
                    imm,
                    sh: carry,
                    ..Insn::new(Kind::DpImm)
                }
            } else if !bit(w, 4) {
                let (sh, amt) = imm_shift(bits(w, 6, 5), bits(w, 11, 7));
                Insn {
                    op,
                    flags,
                    rd,
                    rn,
                    rm,
                    sh,
                    amt,
                    ..Insn::new(Kind::DpRegImm)
                }
            } else {
                if rd == 15 || rn == 15 || rm == 15 || rs == 15 {
                    return undef();
                }
                Insn {
                    op,
                    flags,
                    rd,
                    rn,
                    rm,
                    ra: rs,
                    sh: bits(w, 6, 5) as u8,
                    ..Insn::new(Kind::DpRegReg)
                }
            }
        }
        0b010 | 0b011 => {
            let reg = bit(w, 25);
            if reg && bit(w, 4) {
                return decode_media(w);
            }
            let kind = match (bit(w, 20), bit(w, 22)) {
                (true, false) => Kind::Ldr,
                (true, true) => Kind::Ldrb,
                (false, false) => Kind::Str,
                (false, true) => Kind::Strb,
            };
            let mut i = Insn {
                rd,
                rn,
                flags: puw(w),
                ..Insn::new(kind)
            };
            if reg {
                let (sh, amt) = imm_shift(bits(w, 6, 5), bits(w, 11, 7));
                i.flags |= F_REG;
                i.rm = rm;
                i.sh = sh;
                i.amt = amt;
            } else {
                i.imm = bits(w, 11, 0);
            }
            i
        }
        0b100 => {
            if bit(w, 22) {
                // User-mode register / exception return forms.
                return undef();
            }
            let op = bits(w, 24, 23) as u8;
            let op = match op {
                0b01 => lsm::IA,
                0b11 => lsm::IB,
                0b00 => lsm::DA,
                _ => lsm::DB,
            };
            let list = bits(w, 15, 0);
            if list == 0 {
                return undef();
            }
            Insn {
                rn,
                imm: list,
                op,
                flags: if bit(w, 21) { F_W } else { 0 },
                ..Insn::new(if bit(w, 20) { Kind::Ldm } else { Kind::Stm })
            }
        }
        0b101 => {
            let off = ((w << 8) as i32 >> 6) as u32;
            Insn {
                imm: off,
                ..Insn::new(if bit(w, 24) { Kind::Bl } else { Kind::B })
            }
        }
        0b110 => vfp::decode_ext_load_store(w),
        _ => {
            if bit(w, 24) {
                return Insn {
                    imm: bits(w, 23, 0),
                    ..Insn::new(Kind::Svc)
                };
            }
            decode_coproc(w)
        }
    }
}

/// ARMExpandImm_C: returns (value, carry out or [CARRY_KEEP]).
fn expand_imm(w: u32) -> (u32, u8) {
    let rot = bits(w, 11, 8) * 2;
    let v = bits(w, 7, 0).rotate_right(rot);
    let carry = if rot == 0 {
        CARRY_KEEP
    } else {
        (v >> 31) as u8
    };
    (v, carry)
}

fn msr(mask: u32, flags: u8, rn: u8, imm: u32) -> Insn {
    // Only the APSR fields (f = nzcvq, s = GE) are accessible in user mode.
    // Writes to the control fields are ignored like on hardware.
    let m = (if mask & 8 != 0 { 2 } else { 0 }) | (if mask & 4 != 0 { 1 } else { 0 });
    Insn {
        op: m,
        flags,
        rn,
        imm,
        ..Insn::new(Kind::Msr)
    }
}

/// A5.2.12 miscellaneous instructions, A5.2.7 halfword multiply.
fn decode_misc(w: u32) -> Insn {
    let rn = bits(w, 19, 16) as u8;
    let rd = bits(w, 15, 12) as u8;
    let rs = bits(w, 11, 8) as u8;
    let rm = bits(w, 3, 0) as u8;
    let op = bits(w, 22, 21);
    if bit(w, 7) {
        if bit(w, 4) {
            return undef();
        }
        // Halfword multiply. Note the field positions: Rd is 19:16 and
        // Ra is 15:12.
        let x = bit(w, 5) as u8;
        let y = bit(w, 6) as u8;
        let (mop, sh) = match op {
            0b00 => (1, x | y << 1),
            0b01 => (if x != 0 { 2 } else { 3 }, y << 1),
            0b10 => (4, x | y << 1),
            _ => (0, x | y << 1),
        };
        if rn == 15 || rm == 15 || rs == 15 {
            return undef();
        }
        return Insn {
            op: mop,
            sh,
            rd: if mop == 4 { rd } else { rn },
            ra: if mop == 4 { rn } else { rd },
            rn: rm,
            rm: rs,
            ..Insn::new(Kind::SmulXy)
        };
    }
    match bits(w, 6, 4) {
        0b000 => {
            if op & 1 == 0 {
                if op == 0b10 {
                    return undef(); // MRS SPSR
                }
                Insn {
                    rd,
                    ..Insn::new(Kind::Mrs)
                }
            } else if op == 0b01 {
                msr(bits(w, 19, 16), 0, rm, 0)
            } else {
                undef()
            }
        }
        0b001 if op == 0b01 => Insn {
            rm,
            ..Insn::new(Kind::Bx)
        },
        0b001 if op == 0b11 => Insn {
            rd,
            rm,
            ..Insn::new(Kind::Clz)
        },
        0b011 if op == 0b01 => Insn {
            rm,
            ..Insn::new(Kind::BlxReg)
        },
        0b101 => Insn {
            op: op as u8,
            rd,
            rn,
            rm,
            ..Insn::new(Kind::Qarith)
        },
        0b111 if op == 0b01 => Insn {
            imm: bits(w, 19, 8) << 4 | bits(w, 3, 0),
            ..Insn::new(Kind::Bkpt)
        },
        _ => undef(),
    }
}

/// A5.2.5 multiply, A5.2.10 synchronization, A5.2.8 extra load/store.
fn decode_mul_extra(w: u32) -> Insn {
    let rn = bits(w, 19, 16) as u8;
    let rd = bits(w, 15, 12) as u8;
    let rs = bits(w, 11, 8) as u8;
    let rm = bits(w, 3, 0) as u8;
    let s = if bit(w, 20) { F_S } else { 0 };
    let op2 = bits(w, 6, 5);
    if op2 == 0 {
        if bit(w, 24) {
            // Synchronization primitives.
            if !bit(w, 23) {
                return undef(); // SWP/SWPB
            }
            let size = match bits(w, 22, 21) {
                0b00 => 4,
                0b01 => 8,
                0b10 => 1,
                _ => 2,
            };
            if bit(w, 20) {
                return Insn {
                    op: size,
                    rd,
                    ra: rd + 1,
                    rn,
                    ..Insn::new(Kind::Ldrex)
                };
            }
            return Insn {
                op: size,
                rd,
                rm,
                ra: rm + 1,
                rn,
                ..Insn::new(Kind::Strex)
            };
        }
        // Multiplies: RdHi/Rd is 19:16, RdLo/Ra is 15:12.
        let (kind, op) = match bits(w, 23, 21) {
            0b000 => (Kind::Mul, 0),
            0b001 => (Kind::Mul, 1),
            0b010 if s == 0 => (Kind::MulLong, mul_long::UMAAL),
            0b011 if s == 0 => (Kind::Mul, 2),
            0b100 => (Kind::MulLong, mul_long::UMULL),
            0b101 => (Kind::MulLong, mul_long::UMLAL),
            0b110 => (Kind::MulLong, mul_long::SMULL),
            0b111 => (Kind::MulLong, mul_long::SMLAL),
            _ => return undef(),
        };
        if kind == Kind::Mul {
            return Insn {
                op,
                flags: s,
                rd: rn,
                ra: rd,
                rn: rm,
                rm: rs,
                ..Insn::new(kind)
            };
        }
        return Insn {
            op,
            flags: s,
            rd,
            ra: rn,
            rn: rm,
            rm: rs,
            ..Insn::new(kind)
        };
    }
    // Extra load/store.
    let load = bit(w, 20);
    let kind = match (op2, load) {
        (0b01, false) => Kind::Strh,
        (0b01, true) => Kind::Ldrh,
        (0b10, false) => Kind::Ldrd,
        (0b10, true) => Kind::Ldrsb,
        (0b11, false) => Kind::Strd,
        _ => Kind::Ldrsh,
    };
    if matches!(kind, Kind::Ldrd | Kind::Strd) && (rd & 1 != 0 || rd == 14) {
        return undef();
    }
    let mut i = Insn {
        rd,
        ra: rd + 1,
        rn,
        flags: puw(w),
        ..Insn::new(kind)
    };
    if bit(w, 22) {
        i.imm = bits(w, 11, 8) << 4 | bits(w, 3, 0);
    } else {
        i.flags |= F_REG;
        i.rm = rm;
    }
    i
}

/// A5.4 media instructions.
fn decode_media(w: u32) -> Insn {
    let rn = bits(w, 19, 16) as u8;
    let rd = bits(w, 15, 12) as u8;
    let rs = bits(w, 11, 8) as u8;
    let rm = bits(w, 3, 0) as u8;
    let op1 = bits(w, 24, 20);
    let op2 = bits(w, 7, 5);
    match op1 >> 3 {
        0b00 => {
            // Parallel add/subtract.
            let kind = match op1 & 7 {
                0b001 => 0,
                0b010 => 1,
                0b011 => 2,
                0b101 => 4,
                0b110 => 5,
                0b111 => 6,
                _ => return undef(),
            };
            let sh = match op2 {
                0b000 => par::ADD16,
                0b001 => par::ASX,
                0b010 => par::SAX,
                0b011 => par::SUB16,
                0b100 => par::ADD8,
                0b111 => par::SUB8,
                _ => return undef(),
            };
            Insn {
                op: kind,
                sh,
                rd,
                rn,
                rm,
                ..Insn::new(Kind::ParAddSub)
            }
        }
        0b01 => {
            let rot = (bits(w, 11, 10) * 8) as u8;
            let extend = |op| Insn {
                op,
                rd,
                rn,
                rm,
                amt: rot,
                ..Insn::new(Kind::Extend)
            };
            match (op1 & 7, op2) {
                (0b000, o) if o & 1 == 0 => Insn {
                    rd,
                    rn,
                    rm,
                    amt: bits(w, 11, 7) as u8,
                    sh: bit(w, 6) as u8,
                    ..Insn::new(Kind::Pkh)
                },
                (0b000, 0b011) => extend(ext::SXTB16),
                (0b000, 0b101) => Insn {
                    rd,
                    rn,
                    rm,
                    ..Insn::new(Kind::Sel)
                },
                (0b010 | 0b011, o) if o & 1 == 0 => sat(w, Kind::Ssat, 1),
                (0b010, 0b001) => Insn {
                    rd,
                    rn: rm,
                    imm: bits(w, 19, 16) + 1,
                    ..Insn::new(Kind::Ssat16)
                },
                (0b010, 0b011) => extend(ext::SXTB),
                (0b011, 0b001) => rev(0, rd, rm),
                (0b011, 0b011) => extend(ext::SXTH),
                (0b011, 0b101) => rev(1, rd, rm),
                (0b100, 0b011) => extend(ext::UXTB16),
                (0b110 | 0b111, o) if o & 1 == 0 => sat(w, Kind::Usat, 0),
                (0b110, 0b001) => Insn {
                    rd,
                    rn: rm,
                    imm: bits(w, 19, 16),
                    ..Insn::new(Kind::Usat16)
                },
                (0b110, 0b011) => extend(ext::UXTB),
                (0b111, 0b001) => rev(3, rd, rm),
                (0b111, 0b011) => extend(ext::UXTH),
                (0b111, 0b101) => rev(2, rd, rm),
                _ => undef(),
            }
        }
        0b10 => {
            // Signed multiplies: Rd is 19:16, Ra is 15:12, Rm is 11:8.
            let swap = bit(w, 5) as u8;
            match (op1 & 7, op2 >> 1) {
                (0b000, o @ (0b00 | 0b01)) => Insn {
                    op: o as u8,
                    amt: swap,
                    rd: rn,
                    ra: rd,
                    rn: rm,
                    rm: rs,
                    ..Insn::new(Kind::SmDual)
                },
                (0b100, o @ (0b00 | 0b01)) => Insn {
                    op: 2 + o as u8,
                    amt: swap,
                    rd,
                    ra: rn,
                    rn: rm,
                    rm: rs,
                    ..Insn::new(Kind::SmDual)
                },
                (0b101, o @ (0b00 | 0b11)) => {
                    let op = if o == 0 {
                        if rd == 15 {
                            0
                        } else {
                            1
                        }
                    } else {
                        2
                    };
                    Insn {
                        op,
                        amt: swap,
                        rd: rn,
                        ra: rd,
                        rn: rm,
                        rm: rs,
                        ..Insn::new(Kind::Smmul)
                    }
                }
                _ => undef(),
            }
        }
        _ => match (op1, op2) {
            (0b11000, 0b000) => Insn {
                rd: rn,
                ra: rd,
                rn: rm,
                rm: rs,
                ..Insn::new(Kind::Usad8)
            },
            (0b11010 | 0b11011, o) if o & 3 == 0b10 => bitfield(w, Kind::Sbfx),
            (0b11100 | 0b11101, o) if o & 3 == 0b00 => {
                if rm == 15 {
                    bitfield(w, Kind::Bfc)
                } else {
                    bitfield(w, Kind::Bfi)
                }
            }
            (0b11110 | 0b11111, o) if o & 3 == 0b10 => bitfield(w, Kind::Ubfx),
            _ => undef(),
        },
    }
}

fn rev(op: u8, rd: u8, rm: u8) -> Insn {
    Insn {
        op,
        rd,
        rm,
        ..Insn::new(Kind::Rev)
    }
}

fn sat(w: u32, kind: Kind, plus: u32) -> Insn {
    let (sh, amt) = if bit(w, 6) {
        imm_shift(2, bits(w, 11, 7))
    } else {
        (shift::LSL, bits(w, 11, 7) as u8)
    };
    Insn {
        rd: bits(w, 15, 12) as u8,
        rn: bits(w, 3, 0) as u8,
        imm: bits(w, 20, 16) + plus,
        sh,
        amt,
        ..Insn::new(kind)
    }
}

fn bitfield(w: u32, kind: Kind) -> Insn {
    Insn {
        rd: bits(w, 15, 12) as u8,
        rn: bits(w, 3, 0) as u8,
        amt: bits(w, 11, 7) as u8,
        imm: bits(w, 20, 16),
        ..Insn::new(kind)
    }
}

/// A5.6 coprocessor instructions (other than load/store), cond != 1111.
fn decode_coproc(w: u32) -> Insn {
    let coproc = bits(w, 11, 8);
    if coproc & 0b1110 == 0b1010 {
        return vfp::decode_vfp(w);
    }
    if coproc == 15 && bit(w, 4) {
        let opc1 = bits(w, 23, 21);
        let crn = bits(w, 19, 16);
        let rt = bits(w, 15, 12) as u8;
        let opc2 = bits(w, 7, 5);
        let crm = bits(w, 3, 0);
        if bit(w, 20) {
            if (opc1, crn, crm, opc2) == (0, 13, 0, 3) && rt != 15 {
                return Insn {
                    rd: rt,
                    ..Insn::new(Kind::MrcTpidruro)
                };
            }
        } else if (opc1, crn, crm, opc2) == (0, 7, 10, 5) {
            // CP15DMB. A no-op because there is only one core.
            return Insn::new(Kind::Nop);
        }
    }
    undef()
}

/// A5.7 unconditional instructions.
fn decode_unconditional(w: u32) -> Insn {
    if bits(w, 27, 25) == 0b101 {
        let off = ((w << 8) as i32 >> 6) as u32 | (bit(w, 24) as u32) << 1;
        return Insn {
            imm: off,
            ..Insn::new(Kind::BlxImm)
        };
    }
    if bits(w, 27, 25) == 0b001 {
        return vfp::decode_neon(w);
    }
    // PLD, PLDW, PLI (immediate, literal, register): preload hints.
    if w & 0x0c30_f000 == 0x0410_f000 && !(bit(w, 25) && bit(w, 4)) {
        return Insn::new(Kind::Nop);
    }
    match w & 0xffff_fff0 {
        0xf57f_f010 if w & 0xf == 0xf => return Insn::new(Kind::Clrex),
        // DSB, DMB, ISB.
        0xf57f_f040 | 0xf57f_f050 | 0xf57f_f060 => return Insn::new(Kind::Nop),
        _ => (),
    }
    undef()
}
