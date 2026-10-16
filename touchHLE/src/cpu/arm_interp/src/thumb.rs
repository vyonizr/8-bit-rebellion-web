/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! T32 (Thumb and Thumb-2) decoder.
//!
//! Table references are to the ARMv7-A/R Architecture Reference Manual
//! (DDI 0406C), chapter A6.

use crate::arm;
use crate::insn::*;

fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1 << (hi - lo + 1)) - 1)
}
fn bit(w: u32, b: u32) -> bool {
    (w >> b) & 1 != 0
}
fn sext(v: u32, width: u32) -> u32 {
    ((v << (32 - width)) as i32 >> (32 - width)) as u32
}

fn undef() -> Insn {
    Insn::new(Kind::Undef)
}

/// Whether a halfword is the first half of a 32-bit instruction.
pub fn is_32bit(hw1: u16) -> bool {
    hw1 >> 11 >= 0b11101
}

/// Decodes the instruction at `addr`, executing with ITSTATE `it`. Returns
/// the instruction and the ITSTATE for the next one.
pub fn decode(hw1: u16, hw2: u16, addr: u32, it: u8) -> (Insn, u8) {
    let pc = addr.wrapping_add(4);
    let in_it = it != 0;
    let (mut i, len) = if is_32bit(hw1) {
        (decode32(hw1 as u32, hw2 as u32, in_it, pc), 4)
    } else {
        (decode16(hw1 as u32, in_it, pc), 2)
    };
    i.pc = pc;
    i.len = len;
    i.it = it;
    // Branch targets were decoded as offsets.
    match i.kind {
        Kind::B | Kind::Bl | Kind::Cbz => i.imm = i.imm.wrapping_add(pc),
        Kind::BlxImm => i.imm = i.imm.wrapping_add(pc & !3),
        _ => (),
    }
    // The IT instruction itself is decoded as a NOP with a non-zero `rd` and
    // the new ITSTATE in `imm`, see [Insn::it_after].
    if i.kind == Kind::Nop && i.rd != 0 {
        if in_it {
            return (
                Insn {
                    kind: Kind::Undef,
                    ..i
                },
                0,
            );
        }
        i.imm = hw1 as u32 & 0xff;
        return (i, hw1 as u8);
    }
    if in_it {
        // Conditional branches and CBZ/CBNZ can't be in IT blocks.
        if i.cond != AL || i.kind == Kind::Cbz {
            return (
                Insn {
                    kind: Kind::Undef,
                    cond: AL,
                    ..i
                },
                it_advance(it),
            );
        }
        i.cond = it >> 4;
        // Instructions writing the PC must be last in the block.
        let writes_pc = i.ends_block() && !matches!(i.kind, Kind::Svc | Kind::Bkpt | Kind::Undef);
        if writes_pc && it_advance(it) != 0 {
            return (
                Insn {
                    kind: Kind::Undef,
                    cond: AL,
                    ..i
                },
                it_advance(it),
            );
        }
    }
    (i, it_advance(it))
}

fn dp_imm(op: u8, flags: u8, rd: u8, rn: u8, imm: u32) -> Insn {
    Insn {
        op,
        flags,
        rd,
        rn,
        imm,
        sh: CARRY_KEEP,
        ..Insn::new(Kind::DpImm)
    }
}

fn dp_reg(op: u8, flags: u8, rd: u8, rn: u8, rm: u8) -> Insn {
    Insn {
        op,
        flags,
        rd,
        rn,
        rm,
        ..Insn::new(Kind::DpRegImm)
    }
}

fn ls_imm(kind: Kind, rt: u8, rn: u8, imm: u32) -> Insn {
    Insn {
        rd: rt,
        rn,
        imm,
        flags: F_P | F_U,
        ..Insn::new(kind)
    }
}

/// A6.2 16-bit instructions.
fn decode16(h: u32, in_it: bool, pc: u32) -> Insn {
    let s = if in_it { 0 } else { F_S };
    let r0 = bits(h, 2, 0) as u8;
    let r3 = bits(h, 5, 3) as u8;
    let r6 = bits(h, 8, 6) as u8;
    let r8 = bits(h, 10, 8) as u8;
    let imm8 = bits(h, 7, 0);
    match h >> 11 {
        0b00000..=0b00010 => {
            let (sh, amt) = arm::imm_shift(bits(h, 12, 11), bits(h, 10, 6));
            Insn {
                sh,
                amt,
                ..dp_reg(alu::MOV, s, r0, 0, r3)
            }
        }
        0b00011 => {
            let op = if bit(h, 9) { alu::SUB } else { alu::ADD };
            if bit(h, 10) {
                dp_imm(op, s, r0, r3, r6 as u32)
            } else {
                dp_reg(op, s, r0, r3, r6)
            }
        }
        0b00100 => dp_imm(alu::MOV, s, r8, 0, imm8),
        0b00101 => dp_imm(alu::CMP, F_S, 0, r8, imm8),
        0b00110 => dp_imm(alu::ADD, s, r8, r8, imm8),
        0b00111 => dp_imm(alu::SUB, s, r8, r8, imm8),
        0b01000 => {
            if !bit(h, 10) {
                // Data processing.
                let shift_reg = |sh| Insn {
                    op: alu::MOV,
                    flags: s,
                    rd: r0,
                    rm: r0,
                    ra: r3,
                    sh,
                    ..Insn::new(Kind::DpRegReg)
                };
                return match bits(h, 9, 6) {
                    0b0000 => dp_reg(alu::AND, s, r0, r0, r3),
                    0b0001 => dp_reg(alu::EOR, s, r0, r0, r3),
                    0b0010 => shift_reg(shift::LSL),
                    0b0011 => shift_reg(shift::LSR),
                    0b0100 => shift_reg(shift::ASR),
                    0b0101 => dp_reg(alu::ADC, s, r0, r0, r3),
                    0b0110 => dp_reg(alu::SBC, s, r0, r0, r3),
                    0b0111 => shift_reg(shift::ROR),
                    0b1000 => dp_reg(alu::TST, F_S, 0, r0, r3),
                    0b1001 => dp_imm(alu::RSB, s, r0, r3, 0),
                    0b1010 => dp_reg(alu::CMP, F_S, 0, r0, r3),
                    0b1011 => dp_reg(alu::CMN, F_S, 0, r0, r3),
                    0b1100 => dp_reg(alu::ORR, s, r0, r0, r3),
                    0b1101 => Insn {
                        flags: s,
                        rd: r0,
                        rn: r3,
                        rm: r0,
                        ..Insn::new(Kind::Mul)
                    },
                    0b1110 => dp_reg(alu::BIC, s, r0, r0, r3),
                    _ => dp_reg(alu::MVN, s, r0, 0, r3),
                };
            }
            // Special data instructions and branch and exchange.
            let rdn = (bits(h, 7, 7) << 3) as u8 | r0;
            let rm = bits(h, 6, 3) as u8;
            match bits(h, 9, 8) {
                0b00 => dp_reg(alu::ADD, 0, rdn, rdn, rm),
                0b01 => dp_reg(alu::CMP, F_S, 0, rdn, rm),
                0b10 => dp_reg(alu::MOV, 0, rdn, 0, rm),
                _ => Insn {
                    rm,
                    ..Insn::new(if bit(h, 7) { Kind::BlxReg } else { Kind::Bx })
                },
            }
        }
        0b01001 => ls_imm(Kind::Ldr, r8, 15, imm8 * 4),
        0b01010 | 0b01011 => {
            let kind = [
                Kind::Str,
                Kind::Strh,
                Kind::Strb,
                Kind::Ldrsb,
                Kind::Ldr,
                Kind::Ldrh,
                Kind::Ldrb,
                Kind::Ldrsh,
            ][bits(h, 11, 9) as usize];
            Insn {
                rm: r6,
                flags: F_P | F_U | F_REG,
                ..ls_imm(kind, r0, r3, 0)
            }
        }
        0b01100 => ls_imm(Kind::Str, r0, r3, bits(h, 10, 6) * 4),
        0b01101 => ls_imm(Kind::Ldr, r0, r3, bits(h, 10, 6) * 4),
        0b01110 => ls_imm(Kind::Strb, r0, r3, bits(h, 10, 6)),
        0b01111 => ls_imm(Kind::Ldrb, r0, r3, bits(h, 10, 6)),
        0b10000 => ls_imm(Kind::Strh, r0, r3, bits(h, 10, 6) * 2),
        0b10001 => ls_imm(Kind::Ldrh, r0, r3, bits(h, 10, 6) * 2),
        0b10010 => ls_imm(Kind::Str, r8, 13, imm8 * 4),
        0b10011 => ls_imm(Kind::Ldr, r8, 13, imm8 * 4),
        0b10100 => dp_imm(alu::MOV, 0, r8, 0, (pc & !3).wrapping_add(imm8 * 4)),
        0b10101 => dp_imm(alu::ADD, 0, r8, 13, imm8 * 4),
        0b10110 | 0b10111 => decode16_misc(h),
        0b11000 => Insn {
            rn: r8,
            imm: imm8,
            op: lsm::IA,
            flags: F_W,
            ..Insn::new(if imm8 == 0 { Kind::Undef } else { Kind::Stm })
        },
        0b11001 => Insn {
            rn: r8,
            imm: imm8,
            op: lsm::IA,
            flags: if imm8 & (1 << r8) == 0 { F_W } else { 0 },
            ..Insn::new(if imm8 == 0 { Kind::Undef } else { Kind::Ldm })
        },
        0b11010 | 0b11011 => match bits(h, 11, 8) {
            0b1110 => undef(),
            0b1111 => Insn {
                imm: imm8,
                ..Insn::new(Kind::Svc)
            },
            cond => Insn {
                cond: cond as u8,
                imm: sext(imm8 << 1, 9),
                ..Insn::new(Kind::B)
            },
        },
        0b11100 => Insn {
            imm: sext(bits(h, 10, 0) << 1, 12),
            ..Insn::new(Kind::B)
        },
        _ => undef(),
    }
}

/// A6.2.5 miscellaneous 16-bit instructions.
fn decode16_misc(h: u32) -> Insn {
    let r0 = bits(h, 2, 0) as u8;
    let r3 = bits(h, 5, 3) as u8;
    match bits(h, 11, 8) {
        0b0000 => {
            let op = if bit(h, 7) { alu::SUB } else { alu::ADD };
            dp_imm(op, 0, 13, 13, bits(h, 6, 0) * 4)
        }
        0b0001 | 0b0011 | 0b1001 | 0b1011 => Insn {
            op: bit(h, 11) as u8,
            rn: r0,
            imm: bits(h, 9, 9) << 6 | bits(h, 7, 3) << 1,
            ..Insn::new(Kind::Cbz)
        },
        0b0010 => Insn {
            op: [ext::SXTH, ext::SXTB, ext::UXTH, ext::UXTB][bits(h, 7, 6) as usize],
            rd: r0,
            rn: 15,
            rm: r3,
            ..Insn::new(Kind::Extend)
        },
        0b0100 | 0b0101 => {
            let list = bits(h, 7, 0) | (bits(h, 8, 8) << 14);
            if list == 0 {
                return undef();
            }
            Insn {
                rn: 13,
                imm: list,
                op: lsm::DB,
                flags: F_W,
                ..Insn::new(Kind::Stm)
            }
        }
        0b1010 => match bits(h, 7, 6) {
            0b00 => Insn {
                op: 0,
                rd: r0,
                rm: r3,
                ..Insn::new(Kind::Rev)
            },
            0b01 => Insn {
                op: 1,
                rd: r0,
                rm: r3,
                ..Insn::new(Kind::Rev)
            },
            0b11 => Insn {
                op: 2,
                rd: r0,
                rm: r3,
                ..Insn::new(Kind::Rev)
            },
            _ => undef(),
        },
        0b1100 | 0b1101 => {
            let list = bits(h, 7, 0) | (bits(h, 8, 8) << 15);
            if list == 0 {
                return undef();
            }
            Insn {
                rn: 13,
                imm: list,
                op: lsm::IA,
                flags: F_W,
                ..Insn::new(Kind::Ldm)
            }
        }
        0b1110 => Insn {
            imm: bits(h, 7, 0),
            ..Insn::new(Kind::Bkpt)
        },
        0b1111 => {
            if bits(h, 3, 0) == 0 {
                // Hints.
                Insn::new(Kind::Nop)
            } else if bits(h, 7, 4) == 0b1111
                || (bits(h, 7, 4) == 0b1110 && bits(h, 3, 0).count_ones() != 1)
            {
                // IT with firstcond = 1111, or AL with an "else" slot.
                undef()
            } else {
                // IT. Marked by a non-zero `rd`, see [decode].
                Insn {
                    rd: 1,
                    ..Insn::new(Kind::Nop)
                }
            }
        }
        _ => undef(),
    }
}

/// ThumbExpandImm_C: (value, carry out or [CARRY_KEEP]).
fn expand_imm(imm12: u32) -> (u32, u8) {
    if imm12 >> 10 == 0 {
        let b = imm12 & 0xff;
        let v = match (imm12 >> 8) & 3 {
            0 => b,
            1 => b << 16 | b,
            2 => b << 24 | b << 8,
            _ => b * 0x0101_0101,
        };
        (v, CARRY_KEEP)
    } else {
        let v = (0x80 | (imm12 & 0x7f)).rotate_right(imm12 >> 7);
        (v, (v >> 31) as u8)
    }
}

/// A6.3 32-bit instructions.
fn decode32(hw1: u32, hw2: u32, in_it: bool, pc: u32) -> Insn {
    let rn = bits(hw1, 3, 0) as u8;
    let rd = bits(hw2, 11, 8) as u8;
    let rm = bits(hw2, 3, 0) as u8;
    let rt = bits(hw2, 15, 12) as u8;
    match bits(hw1, 12, 11) {
        0b01 => {
            if bit(hw1, 10) {
                return decode_coproc(hw1, hw2);
            }
            if bit(hw1, 9) {
                return dp_shifted_reg(hw1, hw2);
            }
            if !bit(hw1, 6) {
                // Load/store multiple.
                let op = match bits(hw1, 8, 7) {
                    0b01 => lsm::IA,
                    0b10 => lsm::DB,
                    _ => return undef(),
                };
                let list = hw2 & if bit(hw1, 4) { 0xdfff } else { 0x5fff };
                if list == 0 || list != hw2 {
                    return undef();
                }
                return Insn {
                    rn,
                    imm: list,
                    op,
                    flags: if bit(hw1, 5) { F_W } else { 0 },
                    ..Insn::new(if bit(hw1, 4) { Kind::Ldm } else { Kind::Stm })
                };
            }
            // Load/store dual, exclusive, table branch.
            let (p, u, wb, l) = (bit(hw1, 8), bit(hw1, 7), bit(hw1, 5), bit(hw1, 4));
            if !p && !wb {
                if !u {
                    let imm = bits(hw2, 7, 0) * 4;
                    return if l {
                        Insn {
                            op: 4,
                            rd: rt,
                            rn,
                            imm,
                            ..Insn::new(Kind::Ldrex)
                        }
                    } else {
                        Insn {
                            op: 4,
                            rd,
                            rm: rt,
                            rn,
                            imm,
                            ..Insn::new(Kind::Strex)
                        }
                    };
                }
                let op3 = bits(hw2, 7, 4);
                let size = match op3 {
                    0b0100 => 1,
                    0b0101 => 2,
                    0b0111 => 8,
                    0b0000 | 0b0001 if l => {
                        return Insn {
                            op: op3 as u8,
                            rn,
                            rm,
                            ..Insn::new(Kind::Tbb)
                        };
                    }
                    _ => return undef(),
                };
                return if l {
                    Insn {
                        op: size,
                        rd: rt,
                        ra: rd,
                        rn,
                        ..Insn::new(Kind::Ldrex)
                    }
                } else {
                    Insn {
                        op: size,
                        rd: rm,
                        rm: rt,
                        ra: rd,
                        rn,
                        ..Insn::new(Kind::Strex)
                    }
                };
            }
            let mut flags = 0;
            if p {
                flags |= F_P;
            }
            if u {
                flags |= F_U;
            }
            if wb {
                flags |= F_W;
            }
            Insn {
                rd: rt,
                ra: rd,
                rn,
                imm: bits(hw2, 7, 0) * 4,
                flags,
                ..Insn::new(if l { Kind::Ldrd } else { Kind::Strd })
            }
        }
        0b10 => {
            if bit(hw2, 15) {
                return decode_branch_misc(hw1, hw2, in_it);
            }
            let imm12 = bits(hw1, 10, 10) << 11 | bits(hw2, 14, 12) << 8 | bits(hw2, 7, 0);
            if !bit(hw1, 9) {
                // Data processing (modified immediate).
                let (imm, carry) = expand_imm(imm12);
                let Some(mut i) = dp_op(bits(hw1, 8, 5), bit(hw1, 4), rd, rn) else {
                    return undef();
                };
                i.kind = Kind::DpImm;
                i.imm = imm;
                i.sh = carry;
                return i;
            }
            // Data processing (plain binary immediate).
            let imm5 = bits(hw2, 14, 12) << 2 | bits(hw2, 7, 6);
            let sat_imm = bits(hw2, 4, 0);
            match bits(hw1, 8, 4) {
                0b00000 if rn == 15 => dp_imm(alu::MOV, 0, rd, 0, (pc & !3).wrapping_add(imm12)),
                0b00000 => dp_imm(alu::ADD, 0, rd, rn, imm12),
                0b01010 if rn == 15 => dp_imm(alu::MOV, 0, rd, 0, (pc & !3).wrapping_sub(imm12)),
                0b01010 => dp_imm(alu::SUB, 0, rd, rn, imm12),
                0b00100 | 0b01100 => Insn {
                    rd,
                    imm: (rn as u32) << 12 | imm12,
                    ..Insn::new(if bit(hw1, 7) { Kind::Movt } else { Kind::Movw })
                },
                op @ (0b10000 | 0b10010 | 0b11000 | 0b11010) => {
                    let signed = op & 0b01000 == 0;
                    let asr = op & 0b00010 != 0;
                    if asr && imm5 == 0 {
                        return Insn {
                            rd,
                            rn,
                            imm: if signed {
                                (sat_imm & 15) + 1
                            } else {
                                sat_imm & 15
                            },
                            ..Insn::new(if signed { Kind::Ssat16 } else { Kind::Usat16 })
                        };
                    }
                    Insn {
                        rd,
                        rn,
                        imm: if signed { sat_imm + 1 } else { sat_imm },
                        sh: if asr { shift::ASR } else { shift::LSL },
                        amt: imm5 as u8,
                        ..Insn::new(if signed { Kind::Ssat } else { Kind::Usat })
                    }
                }
                op @ (0b10100 | 0b11100 | 0b10110) => Insn {
                    rd,
                    rn,
                    amt: imm5 as u8,
                    imm: sat_imm,
                    ..Insn::new(match op {
                        0b10100 => Kind::Sbfx,
                        0b11100 => Kind::Ubfx,
                        _ if rn == 15 => Kind::Bfc,
                        _ => Kind::Bfi,
                    })
                },
                _ => undef(),
            }
        }
        _ => {
            if bit(hw1, 10) {
                return decode_coproc(hw1, hw2);
            }
            match bits(hw1, 10, 8) {
                0b000 | 0b001 => {
                    if !bit(hw1, 4) && bit(hw1, 8) {
                        return undef(); // Advanced SIMD element load/store.
                    }
                    decode_load_store(hw1, hw2)
                }
                0b010 => decode_dp_reg(hw1, hw2),
                _ if !bit(hw1, 7) => decode_mul(hw1, hw2),
                _ => decode_mul_long(hw1, hw2),
            }
        }
    }
}

/// Maps a T32 data processing opcode to an ALU op, handling the TST/TEQ/
/// CMN/CMP and MOV/MVN aliases. Returns an instruction with only `op`,
/// `flags`, `rd`, `rn` set.
fn dp_op(op: u32, s: bool, rd: u8, rn: u8) -> Option<Insn> {
    let flags = if s { F_S } else { 0 };
    let (op, rd, rn) = match op {
        0b0000 if rd == 15 && s => (alu::TST, 0, rn),
        0b0000 => (alu::AND, rd, rn),
        0b0001 => (alu::BIC, rd, rn),
        0b0010 if rn == 15 => (alu::MOV, rd, 0),
        0b0010 => (alu::ORR, rd, rn),
        0b0011 if rn == 15 => (alu::MVN, rd, 0),
        0b0011 => (alu::ORN, rd, rn),
        0b0100 if rd == 15 && s => (alu::TEQ, 0, rn),
        0b0100 => (alu::EOR, rd, rn),
        0b1000 if rd == 15 && s => (alu::CMN, 0, rn),
        0b1000 => (alu::ADD, rd, rn),
        0b1010 => (alu::ADC, rd, rn),
        0b1011 => (alu::SBC, rd, rn),
        0b1101 if rd == 15 && s => (alu::CMP, 0, rn),
        0b1101 => (alu::SUB, rd, rn),
        0b1110 => (alu::RSB, rd, rn),
        _ => return None,
    };
    // Thumb-2 data processing can't write the PC.
    if rd == 15 {
        return None;
    }
    Some(Insn {
        op,
        flags,
        rd,
        rn,
        ..Insn::new(Kind::DpImm)
    })
}

/// A6.3.11 data processing (shifted register).
fn dp_shifted_reg(hw1: u32, hw2: u32) -> Insn {
    let rn = bits(hw1, 3, 0) as u8;
    let rd = bits(hw2, 11, 8) as u8;
    let rm = bits(hw2, 3, 0) as u8;
    let imm5 = bits(hw2, 14, 12) << 2 | bits(hw2, 7, 6);
    let op = bits(hw1, 8, 5);
    if op == 0b0110 {
        if bit(hw1, 4) || bit(hw2, 4) {
            return undef();
        }
        return Insn {
            rd,
            rn,
            rm,
            amt: imm5 as u8,
            sh: bit(hw2, 5) as u8,
            ..Insn::new(Kind::Pkh)
        };
    }
    let Some(mut i) = dp_op(op, bit(hw1, 4), rd, rn) else {
        return undef();
    };
    let (sh, amt) = arm::imm_shift(bits(hw2, 5, 4), imm5);
    i.kind = Kind::DpRegImm;
    i.rm = rm;
    i.sh = sh;
    i.amt = amt;
    i
}

/// A6.3.4 branches and miscellaneous control.
fn decode_branch_misc(hw1: u32, hw2: u32, in_it: bool) -> Insn {
    let s = bits(hw1, 10, 10);
    let j1 = bits(hw2, 13, 13);
    let j2 = bits(hw2, 11, 11);
    let imm11 = bits(hw2, 10, 0);
    let i1 = 1 ^ j1 ^ s;
    let i2 = 1 ^ j2 ^ s;
    match (bit(hw2, 14), bit(hw2, 12)) {
        (false, false) => {
            if bits(hw1, 9, 7) != 0b111 {
                if in_it {
                    return undef();
                }
                let off = s << 20 | j2 << 19 | j1 << 18 | bits(hw1, 5, 0) << 12 | imm11 << 1;
                return Insn {
                    cond: bits(hw1, 9, 6) as u8,
                    imm: sext(off, 21),
                    ..Insn::new(Kind::B)
                };
            }
            let op = bits(hw1, 10, 4);
            match op {
                0b0111000 | 0b0111001 if !bit(hw1, 4) => Insn {
                    op: (bit(hw2, 11) as u8) << 1 | bit(hw2, 10) as u8,
                    rn: bits(hw1, 3, 0) as u8,
                    ..Insn::new(Kind::Msr)
                },
                0b0111010 if bits(hw2, 10, 8) == 0 => Insn::new(Kind::Nop),
                0b0111011 => match bits(hw2, 7, 4) {
                    0b0010 => Insn::new(Kind::Clrex),
                    0b0100..=0b0110 => Insn::new(Kind::Nop),
                    _ => undef(),
                },
                0b0111110 | 0b0111111 if !bit(hw1, 4) => Insn {
                    rd: bits(hw2, 11, 8) as u8,
                    ..Insn::new(Kind::Mrs)
                },
                _ => undef(),
            }
        }
        (false, true) => {
            let off = s << 24 | i1 << 23 | i2 << 22 | bits(hw1, 9, 0) << 12 | imm11 << 1;
            Insn {
                imm: sext(off, 25),
                ..Insn::new(Kind::B)
            }
        }
        (true, link_only) => {
            let off = s << 24 | i1 << 23 | i2 << 22 | bits(hw1, 9, 0) << 12 | imm11 << 1;
            if link_only {
                Insn {
                    imm: sext(off, 25),
                    ..Insn::new(Kind::Bl)
                }
            } else {
                if hw2 & 1 != 0 {
                    return undef();
                }
                Insn {
                    imm: sext(off, 25),
                    ..Insn::new(Kind::BlxImm)
                }
            }
        }
    }
}

/// A6.3.10 store single data item, A6.3.7-9 loads.
fn decode_load_store(hw1: u32, hw2: u32) -> Insn {
    let rn = bits(hw1, 3, 0) as u8;
    let rt = bits(hw2, 15, 12) as u8;
    let load = bit(hw1, 4);
    let signed = bit(hw1, 8);
    let size = bits(hw1, 6, 5);
    let kind = match (load, signed, size) {
        (false, false, 0) => Kind::Strb,
        (false, false, 1) => Kind::Strh,
        (false, false, 2) => Kind::Str,
        (true, false, 0) => Kind::Ldrb,
        (true, false, 1) => Kind::Ldrh,
        (true, false, 2) => Kind::Ldr,
        (true, true, 0) => Kind::Ldrsb,
        (true, true, 1) => Kind::Ldrsh,
        _ => return undef(),
    };
    if !load && rn == 15 {
        return undef();
    }
    // Rt = PC on byte/halfword loads are the PLD/PLI preload hints.
    if load && rt == 15 && kind != Kind::Ldr {
        return Insn::new(Kind::Nop);
    }
    let mut i = Insn {
        rd: rt,
        rn,
        ..Insn::new(kind)
    };
    if rn == 15 {
        i.imm = bits(hw2, 11, 0);
        i.flags = F_P | if bit(hw1, 7) { F_U } else { 0 };
    } else if bit(hw1, 7) {
        i.imm = bits(hw2, 11, 0);
        i.flags = F_P | F_U;
    } else if bit(hw2, 11) {
        // P, U, W in hw2 bits 10:8.
        let puw = bits(hw2, 10, 8);
        if puw & 0b101 == 0 {
            return undef();
        }
        i.imm = bits(hw2, 7, 0);
        i.flags = (if puw & 4 != 0 { F_P } else { 0 })
            | (if puw & 2 != 0 { F_U } else { 0 })
            | (if puw & 1 != 0 { F_W } else { 0 });
    } else if bits(hw2, 11, 6) == 0 {
        i.flags = F_P | F_U | F_REG;
        i.rm = bits(hw2, 3, 0) as u8;
        i.sh = shift::LSL;
        i.amt = bits(hw2, 5, 4) as u8;
    } else {
        return undef();
    }
    i
}

/// A6.3.12 data processing (register), A6.3.13-16.
fn decode_dp_reg(hw1: u32, hw2: u32) -> Insn {
    let rn = bits(hw1, 3, 0) as u8;
    let rd = bits(hw2, 11, 8) as u8;
    let rm = bits(hw2, 3, 0) as u8;
    if bits(hw2, 15, 12) != 0b1111 {
        return undef();
    }
    if !bit(hw1, 7) {
        if bits(hw2, 7, 4) == 0 {
            return Insn {
                op: alu::MOV,
                flags: if bit(hw1, 4) { F_S } else { 0 },
                rd,
                rm: rn,
                ra: rm,
                sh: bits(hw1, 6, 5) as u8,
                ..Insn::new(Kind::DpRegReg)
            };
        }
        if bit(hw2, 7) {
            let op = match bits(hw1, 6, 4) {
                0b000 => ext::SXTH,
                0b001 => ext::UXTH,
                0b010 => ext::SXTB16,
                0b011 => ext::UXTB16,
                0b100 => ext::SXTB,
                0b101 => ext::UXTB,
                _ => return undef(),
            };
            return Insn {
                op,
                rd,
                rn,
                rm,
                amt: (bits(hw2, 5, 4) * 8) as u8,
                ..Insn::new(Kind::Extend)
            };
        }
        return undef();
    }
    match bits(hw2, 7, 6) {
        0b00 | 0b01 => {
            let sh = match bits(hw1, 6, 4) {
                0b001 => par::ADD16,
                0b010 => par::ASX,
                0b110 => par::SAX,
                0b101 => par::SUB16,
                0b000 => par::ADD8,
                0b100 => par::SUB8,
                _ => return undef(),
            };
            let op2 = bits(hw2, 5, 4);
            if op2 == 3 {
                return undef();
            }
            Insn {
                op: (bit(hw2, 6) as u8) << 2 | op2 as u8,
                sh,
                rd,
                rn,
                rm,
                ..Insn::new(Kind::ParAddSub)
            }
        }
        0b10 if !bit(hw1, 6) => {
            let op2 = bits(hw2, 5, 4) as u8;
            match bits(hw1, 5, 4) {
                0b00 => Insn {
                    op: [0, 2, 1, 3][op2 as usize],
                    rd,
                    rn,
                    rm,
                    ..Insn::new(Kind::Qarith)
                },
                0b01 => Insn {
                    op: [0, 1, 3, 2][op2 as usize],
                    rd,
                    rm,
                    ..Insn::new(Kind::Rev)
                },
                0b10 if op2 == 0 => Insn {
                    rd,
                    rn,
                    rm,
                    ..Insn::new(Kind::Sel)
                },
                0b11 if op2 == 0 => Insn {
                    rd,
                    rm,
                    ..Insn::new(Kind::Clz)
                },
                _ => undef(),
            }
        }
        _ => undef(),
    }
}

/// A6.3.16 multiply, multiply accumulate, and absolute difference.
fn decode_mul(hw1: u32, hw2: u32) -> Insn {
    let rn = bits(hw1, 3, 0) as u8;
    let ra = bits(hw2, 15, 12) as u8;
    let rd = bits(hw2, 11, 8) as u8;
    let rm = bits(hw2, 3, 0) as u8;
    let op2 = bits(hw2, 5, 4);
    let base = |kind| Insn {
        rd,
        rn,
        rm,
        ra,
        ..Insn::new(kind)
    };
    match bits(hw1, 6, 4) {
        0b000 => match op2 {
            0b00 if ra == 15 => base(Kind::Mul),
            0b00 => Insn {
                op: 1,
                ..base(Kind::Mul)
            },
            0b01 => Insn {
                op: 2,
                ..base(Kind::Mul)
            },
            _ => undef(),
        },
        0b001 => Insn {
            op: if ra == 15 { 0 } else { 1 },
            sh: (bit(hw2, 5) as u8) | (bit(hw2, 4) as u8) << 1,
            ..base(Kind::SmulXy)
        },
        0b010 | 0b100 if !bit(hw2, 5) => Insn {
            op: if bit(hw1, 6) { 1 } else { 0 },
            amt: bit(hw2, 4) as u8,
            ..base(Kind::SmDual)
        },
        0b011 if !bit(hw2, 5) => Insn {
            op: if ra == 15 { 2 } else { 3 },
            sh: (bit(hw2, 4) as u8) << 1,
            ..base(Kind::SmulXy)
        },
        0b101 if !bit(hw2, 5) => Insn {
            op: if ra == 15 { 0 } else { 1 },
            amt: bit(hw2, 4) as u8,
            ..base(Kind::Smmul)
        },
        0b110 if !bit(hw2, 5) => Insn {
            op: 2,
            amt: bit(hw2, 4) as u8,
            ..base(Kind::Smmul)
        },
        0b111 if op2 == 0 => base(Kind::Usad8),
        _ => undef(),
    }
}

/// A6.3.17 long multiply, long multiply accumulate, and divide.
fn decode_mul_long(hw1: u32, hw2: u32) -> Insn {
    let base = |kind, op| Insn {
        op,
        rd: bits(hw2, 15, 12) as u8,
        ra: bits(hw2, 11, 8) as u8,
        rn: bits(hw1, 3, 0) as u8,
        rm: bits(hw2, 3, 0) as u8,
        ..Insn::new(kind)
    };
    let op2 = bits(hw2, 7, 4);
    match (bits(hw1, 6, 4), op2) {
        (0b000, 0) => base(Kind::MulLong, mul_long::SMULL),
        (0b010, 0) => base(Kind::MulLong, mul_long::UMULL),
        (0b100, 0) => base(Kind::MulLong, mul_long::SMLAL),
        (0b110, 0) => base(Kind::MulLong, mul_long::UMLAL),
        (0b110, 0b0110) => base(Kind::MulLong, mul_long::UMAAL),
        (0b100, o) if o & 0b1100 == 0b1000 => Insn {
            sh: (bit(o, 1) as u8) | (bit(o, 0) as u8) << 1,
            ..base(Kind::SmulXy, 4)
        },
        (0b100 | 0b101, o) if o & 0b1110 == 0b1100 => Insn {
            amt: (o & 1) as u8,
            ..base(Kind::SmDual, if bit(hw1, 4) { 3 } else { 2 })
        },
        _ => undef(),
    }
}

/// Coprocessor and Advanced SIMD instructions, converted to A32 layout.
fn decode_coproc(hw1: u32, hw2: u32) -> Insn {
    let rest = (hw1 & 0xff) << 16 | hw2;
    if hw1 >> 8 & 0xef == 0xef {
        // Advanced SIMD data processing: 111U 1111 -> 1111 001U.
        let u = bit(hw1, 12) as u32;
        return crate::vfp::decode_neon(0xf200_0000 | u << 24 | rest);
    }
    if hw1 >> 12 != 0b1110 {
        return undef();
    }
    arm::decode_inner(0xe000_0000 | (hw1 & 0x0f00) << 16 | rest)
}
