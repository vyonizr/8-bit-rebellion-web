/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The decoded instruction format shared by the A32 and T32 decoders.
//!
//! Both instruction sets decode into the same flat [Insn] struct so that a
//! single `match` in the executor handles both. Field meanings depend on the
//! [Kind] and are documented next to each kind.

/// Instruction kinds. Field usage is noted per kind; unlisted fields are
/// unused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Raises an undefined instruction exception.
    Undef,
    Nop,
    /// `imm`: SVC immediate.
    Svc,
    Bkpt,
    /// `op`: [Alu], `rd`, `rn`, `imm`: operand 2, `sh`: shifter carry out
    /// (0, 1, or [CARRY_KEEP]).
    DpImm,
    /// `op`: [Alu], `rd`, `rn`, `rm` shifted by `sh` ([Shift]) `amt`.
    DpRegImm,
    /// `op`: [Alu], `rd`, `rn`, `rm` shifted by `sh` by register `ra`.
    DpRegReg,
    /// `rd`, `imm`.
    Movw,
    Movt,
    /// `op`: 0 MUL, 1 MLA, 2 MLS. `rd` = `rn` * `rm` (+/- `ra`).
    Mul,
    /// `op`: [MulLong]. `rd`: RdLo, `ra`: RdHi.
    MulLong,
    /// `op`: 0 SMMUL, 1 SMMLA, 2 SMMLS; `amt`: round. `rd`, `rn`, `rm`, `ra`.
    Smmul,
    /// `op`: 0 SMULxy, 1 SMLAxy, 2 SMULWy, 3 SMLAWy, 4 SMLALxy (`rd`: lo,
    /// `ra`: hi). `sh`: bit 0 = N top half (x), bit 1 = M top half (y).
    SmulXy,
    /// `op`: 0 SMUAD/SMLAD, 1 SMUSD/SMLSD, 2 SMLALD, 3 SMLSLD. `amt`: swap M
    /// halves. `ra` = 15 means no accumulate (32-bit forms only).
    SmDual,
    /// `op`: 0 QADD, 1 QSUB, 2 QDADD, 3 QDSUB. `rd` = `rm` op `rn`.
    Qarith,
    /// `rd` = sat(`rn` shifted by `sh`/`amt`) to `imm` bits.
    Ssat,
    Usat,
    Ssat16,
    Usat16,
    /// `op`: [Ext]. `rd` = `rn` (unless 15) + extend(ror(`rm`, `amt`)).
    Extend,
    /// `op`: 0 REV, 1 REV16, 2 REVSH, 3 RBIT.
    Rev,
    Clz,
    /// `rd`, `amt`: lsb, `imm`: msb.
    Bfc,
    /// `rd`, `rn`, `amt`: lsb, `imm`: msb.
    Bfi,
    /// `rd`, `rn`, `amt`: lsb, `imm`: width - 1.
    Ubfx,
    Sbfx,
    Sel,
    /// `op`: kind (0 S, 1 Q, 2 SH, 4 U, 5 UQ, 6 UH), `sh`: [Par].
    ParAddSub,
    /// `amt`: shift; `sh`: 0 PKHBT, 1 PKHTB.
    Pkh,
    /// `ra` = 15 for USAD8.
    Usad8,
    /// Single loads/stores: `rd`: Rt, `rn`: base, offset `imm` or `rm`
    /// shifted by `sh`/`amt` when [F_REG]. [F_P], [F_U], [F_W] as usual.
    Ldr,
    Ldrb,
    Ldrh,
    Ldrsb,
    Ldrsh,
    Str,
    Strb,
    Strh,
    /// Like the above, `ra`: Rt2.
    Ldrd,
    Strd,
    /// `rn`, `imm`: register list, `op`: [Lsm] mode, [F_W].
    Ldm,
    Stm,
    /// `op`: size in bytes (1, 2, 4, 8). `rd`: Rt, `ra`: Rt2, `rn`, `imm`.
    Ldrex,
    /// `op`: size. `rd`: status, `rm`: Rt, `ra`: Rt2, `rn`, `imm`.
    Strex,
    Clrex,
    /// `imm`: target.
    B,
    /// `imm`: target. LR is set to the next instruction.
    Bl,
    /// `imm`: target, switching instruction set.
    BlxImm,
    /// `rm`.
    Bx,
    BlxReg,
    /// `rn`, `imm`: target, `op`: 1 for CBNZ.
    Cbz,
    /// `rn`, `rm`, `op`: 1 for TBH.
    Tbb,
    /// `rd`.
    Mrs,
    /// `rn` or `imm` if [F_IMM]; `op`: mask (bit 1 = nzcvq, bit 0 = GE).
    Msr,
    /// `rd`.
    MrcTpidruro,
    /// VFP data processing: `op`: [VfpOp], `rd`, `rn`, `rm` are S or D
    /// register numbers depending on [F_DBL].
    Vfp,
    /// VMOV core <-> single: `rd`: core, `rn`: S, `op`: 1 = to core.
    VmovCoreS,
    /// VMOV two core <-> two singles / one double: `rd`, `ra`: core regs,
    /// `rm`: S/D, [F_DBL], `op`: 1 = to core.
    VmovCore2,
    /// VMOV.32 core <-> scalar: `rd`: core, `rn`: D reg, `amt`: index,
    /// `op`: 1 = to core.
    VmovScalar,
    /// `rd`: S/D, `rn`: base, `imm`: offset, [F_U], [F_DBL].
    Vldr,
    Vstr,
    /// `rd`: first S/D, `imm`: count, `rn`: base, `op`: [Lsm], [F_W], [F_DBL].
    Vldm,
    Vstm,
    /// `rd`: core (15 = APSR_nzcv).
    Vmrs,
    Vmsr,
    /// Advanced SIMD VADD/VSUB/VMUL.F32: `op`: 0 add 1 sub 2 mul, `rd`, `rn`,
    /// `rm`: D regs, `imm`: number of D regs (1 or 2).
    NeonF32,
}

pub const F_S: u8 = 1;
pub const F_P: u8 = 2;
pub const F_U: u8 = 4;
pub const F_W: u8 = 8;
pub const F_REG: u8 = 16;
pub const F_DBL: u8 = 32;
pub const F_IMM: u8 = 64;

/// In [Kind::DpImm], `sh` value meaning the shifter carry is unchanged.
pub const CARRY_KEEP: u8 = 2;

/// Condition code meaning "always".
pub const AL: u8 = 14;

/// ALU operations, numbered like the A32 data processing opcodes.
pub mod alu {
    pub const AND: u8 = 0;
    pub const EOR: u8 = 1;
    pub const SUB: u8 = 2;
    pub const RSB: u8 = 3;
    pub const ADD: u8 = 4;
    pub const ADC: u8 = 5;
    pub const SBC: u8 = 6;
    pub const RSC: u8 = 7;
    pub const TST: u8 = 8;
    pub const TEQ: u8 = 9;
    pub const CMP: u8 = 10;
    pub const CMN: u8 = 11;
    pub const ORR: u8 = 12;
    pub const MOV: u8 = 13;
    pub const BIC: u8 = 14;
    pub const MVN: u8 = 15;
    pub const ORN: u8 = 16;
}

/// Shift types. [shift::RRX] is ROR #0 in immediate shifts.
pub mod shift {
    pub const LSL: u8 = 0;
    pub const LSR: u8 = 1;
    pub const ASR: u8 = 2;
    pub const ROR: u8 = 3;
    pub const RRX: u8 = 4;
}

/// [Kind::MulLong] operations.
pub mod mul_long {
    pub const UMULL: u8 = 0;
    pub const UMLAL: u8 = 1;
    pub const SMULL: u8 = 2;
    pub const SMLAL: u8 = 3;
    pub const UMAAL: u8 = 4;
}

/// [Kind::Extend] operations.
pub mod ext {
    pub const SXTB: u8 = 0;
    pub const SXTH: u8 = 1;
    pub const UXTB: u8 = 2;
    pub const UXTH: u8 = 3;
    pub const SXTB16: u8 = 4;
    pub const UXTB16: u8 = 5;
}

/// [Kind::ParAddSub] operations (`sh` field).
pub mod par {
    pub const ADD16: u8 = 0;
    pub const ASX: u8 = 1;
    pub const SAX: u8 = 2;
    pub const SUB16: u8 = 3;
    pub const ADD8: u8 = 4;
    pub const SUB8: u8 = 5;
}

/// Load/store multiple modes.
pub mod lsm {
    pub const IA: u8 = 0;
    pub const IB: u8 = 1;
    pub const DA: u8 = 2;
    pub const DB: u8 = 3;
}

/// [Kind::Vfp] operations.
pub mod vfp_op {
    pub const MLA: u8 = 0;
    pub const MLS: u8 = 1;
    pub const NMLS: u8 = 2;
    pub const NMLA: u8 = 3;
    pub const MUL: u8 = 4;
    pub const NMUL: u8 = 5;
    pub const ADD: u8 = 6;
    pub const SUB: u8 = 7;
    pub const DIV: u8 = 8;
    /// `imm`: the value (high word for doubles).
    pub const MOV_IMM: u8 = 9;
    pub const MOV: u8 = 10;
    pub const ABS: u8 = 11;
    pub const NEG: u8 = 12;
    pub const SQRT: u8 = 13;
    /// `amt`: 1 for VCMPE, `sh`: 1 to compare with zero.
    pub const CMP: u8 = 14;
    /// Between single and double; [super::F_DBL] describes the source.
    pub const CVT_FP: u8 = 15;
    /// Integer (in `rm`, an S register) to float; `amt`: 1 if signed.
    pub const CVT_FROM_INT: u8 = 16;
    /// Float to integer (in `rd`, an S register); `amt`: 1 if signed,
    /// `sh`: 1 to round towards zero rather than use FPSCR.
    pub const CVT_TO_INT: u8 = 17;
}

/// A decoded instruction. 20 bytes.
#[derive(Clone, Copy, Debug)]
pub struct Insn {
    pub kind: Kind,
    /// Condition code, already resolved from IT state for Thumb.
    pub cond: u8,
    /// ITSTATE before this instruction executes (0 outside IT blocks).
    pub it: u8,
    /// Length in bytes (2 or 4).
    pub len: u8,
    pub rd: u8,
    pub rn: u8,
    pub rm: u8,
    pub ra: u8,
    pub op: u8,
    pub flags: u8,
    pub sh: u8,
    pub amt: u8,
    pub imm: u32,
    /// The value reading PC gives: the address plus 8 (A32) or 4 (T32).
    pub pc: u32,
}

impl Insn {
    pub fn new(kind: Kind) -> Insn {
        Insn {
            kind,
            cond: AL,
            it: 0,
            len: 4,
            rd: 0,
            rn: 0,
            rm: 0,
            ra: 0,
            op: 0,
            flags: 0,
            sh: 0,
            amt: 0,
            imm: 0,
            pc: 0,
        }
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// ITSTATE after this instruction executes (without branching).
    pub fn it_after(&self) -> u8 {
        if self.kind == Kind::Nop && self.rd != 0 {
            // An IT instruction.
            self.imm as u8
        } else {
            it_advance(self.it)
        }
    }

    /// Whether this instruction may change the PC or stop execution, so it
    /// must be the last one in a block.
    pub fn ends_block(&self) -> bool {
        match self.kind {
            Kind::Undef
            | Kind::Svc
            | Kind::Bkpt
            | Kind::B
            | Kind::Bl
            | Kind::BlxImm
            | Kind::Bx
            | Kind::BlxReg
            | Kind::Cbz
            | Kind::Tbb => true,
            Kind::DpImm | Kind::DpRegImm | Kind::DpRegReg => {
                self.rd == 15 && !(alu::TST..=alu::CMN).contains(&self.op)
            }
            Kind::Ldr => self.rd == 15,
            Kind::Ldm => self.imm & 0x8000 != 0,
            _ => false,
        }
    }
}

/// Advance ITSTATE after executing an instruction in an IT block.
pub fn it_advance(it: u8) -> u8 {
    if it & 7 == 0 {
        0
    } else {
        (it & 0xe0) | ((it << 1) & 0x1f)
    }
}
