/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! VFPv3 and the Advanced SIMD (NEON) subset the guest uses: decoding,
//! execution, and an IEEE 754 implementation that follows the FPSCR rounding
//! mode, flush-to-zero and default-NaN bits and sets the cumulative exception
//! flags like the ARM pseudocode (`FPRound` etc.).
//!
//! Every arithmetic operation has a fast path using host floats for the
//! common case (round to nearest, normal inputs and output), and a slow path
//! in integer arithmetic for everything else. Host NaN behavior is never
//! relied on, because it differs between x86 and WebAssembly.
//!
//! Decoders take instructions in A32 layout; the T32 decoder converts its
//! coprocessor and Advanced SIMD encodings to that layout first.

use crate::insn::*;
use crate::Cpu;

fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1 << (hi - lo + 1)) - 1)
}
fn bit(w: u32, b: u32) -> bool {
    (w >> b) & 1 != 0
}

fn undef() -> Insn {
    Insn::new(Kind::Undef)
}

/// Register number for a single-precision (`Vx:X`) or double-precision
/// (`X:Vx`) operand.
fn vreg(dbl: bool, v: u32, x: bool) -> u8 {
    if dbl {
        (v | (x as u32) << 4) as u8
    } else {
        (v << 1 | x as u32) as u8
    }
}

/// A7.5 VFP data-processing and A7.8 8/16/32-bit register transfers.
pub fn decode_vfp(w: u32) -> Insn {
    let dbl = bit(w, 8);
    let fl = if dbl { F_DBL } else { 0 };
    let vd = bits(w, 15, 12);
    let vn = bits(w, 19, 16);
    let vm = bits(w, 3, 0);
    let (d, n, m) = (bit(w, 22), bit(w, 7), bit(w, 5));

    if bit(w, 4) {
        let rt = vd as u8;
        let a = bits(w, 23, 21);
        let l = bit(w, 20);
        if !dbl && a == 0 && bits(w, 6, 5) == 0 {
            if rt == 15 {
                return undef();
            }
            return Insn {
                rd: rt,
                rn: vreg(false, vn, n),
                op: l as u8,
                ..Insn::new(Kind::VmovCoreS)
            };
        }
        if !dbl && a == 0b111 && vn == 0b0001 {
            if l {
                return Insn {
                    rd: rt,
                    ..Insn::new(Kind::Vmrs)
                };
            }
            if rt == 15 {
                return undef();
            }
            return Insn {
                rn: rt,
                ..Insn::new(Kind::Vmsr)
            };
        }
        // VMOV.32 between a core register and a scalar.
        if dbl && a & 0b110 == 0 && bits(w, 6, 5) == 0 && rt != 15 {
            return Insn {
                rd: rt,
                rn: vreg(true, vn, n),
                amt: bit(w, 21) as u8,
                op: l as u8,
                ..Insn::new(Kind::VmovScalar)
            };
        }
        return undef();
    }

    let opc1 = (bit(w, 23) as u32) << 2 | bits(w, 21, 20);
    let op6 = bit(w, 6);
    let three = |op| Insn {
        op,
        flags: fl,
        rd: vreg(dbl, vd, d),
        rn: vreg(dbl, vn, n),
        rm: vreg(dbl, vm, m),
        ..Insn::new(Kind::Vfp)
    };
    match opc1 {
        0b000 => three(if op6 { vfp_op::MLS } else { vfp_op::MLA }),
        0b001 => three(if op6 { vfp_op::NMLA } else { vfp_op::NMLS }),
        0b010 => three(if op6 { vfp_op::NMUL } else { vfp_op::MUL }),
        0b011 => three(if op6 { vfp_op::SUB } else { vfp_op::ADD }),
        0b100 if !op6 => three(vfp_op::DIV),
        0b111 => {
            if !op6 {
                // VMOV immediate (VFPExpandImm).
                let imm8 = vn << 4 | vm;
                let sign = imm8 >> 7;
                let b6 = (imm8 >> 6) & 1;
                let imm = if dbl {
                    sign << 31
                        | (b6 ^ 1) << 30
                        | if b6 != 0 { 0xff << 22 } else { 0 }
                        | (imm8 & 0x3f) << 16
                } else {
                    sign << 31
                        | (b6 ^ 1) << 30
                        | if b6 != 0 { 0x1f << 25 } else { 0 }
                        | (imm8 & 0x3f) << 19
                };
                return Insn {
                    op: vfp_op::MOV_IMM,
                    flags: fl,
                    rd: vreg(dbl, vd, d),
                    imm,
                    ..Insn::new(Kind::Vfp)
                };
            }
            let op7 = bit(w, 7);
            match vn {
                0b0000 => three(if op7 { vfp_op::ABS } else { vfp_op::MOV }),
                0b0001 => three(if op7 { vfp_op::SQRT } else { vfp_op::NEG }),
                0b0100 | 0b0101 => Insn {
                    amt: op7 as u8,
                    sh: (vn & 1) as u8,
                    ..three(vfp_op::CMP)
                },
                0b0111 if op7 => Insn {
                    op: vfp_op::CVT_FP,
                    flags: fl,
                    rd: vreg(!dbl, vd, d),
                    rm: vreg(dbl, vm, m),
                    ..Insn::new(Kind::Vfp)
                },
                0b1000 => Insn {
                    op: vfp_op::CVT_FROM_INT,
                    flags: fl,
                    amt: op7 as u8,
                    rd: vreg(dbl, vd, d),
                    rm: vreg(false, vm, m),
                    ..Insn::new(Kind::Vfp)
                },
                0b1100 | 0b1101 => Insn {
                    op: vfp_op::CVT_TO_INT,
                    flags: fl,
                    amt: (vn & 1) as u8,
                    sh: op7 as u8,
                    rd: vreg(false, vd, d),
                    rm: vreg(dbl, vm, m),
                    ..Insn::new(Kind::Vfp)
                },
                _ => undef(),
            }
        }
        _ => undef(),
    }
}

/// A7.6 extension register load/store and A7.9 64-bit transfers.
pub fn decode_ext_load_store(w: u32) -> Insn {
    if bits(w, 11, 9) != 0b101 {
        return undef();
    }
    let dbl = bit(w, 8);
    let fl = if dbl { F_DBL } else { 0 };
    let rn = bits(w, 19, 16) as u8;
    let vd = vreg(dbl, bits(w, 15, 12), bit(w, 22));
    let imm8 = bits(w, 7, 0);
    let (p, u, wb, l) = (bit(w, 24), bit(w, 23), bit(w, 21), bit(w, 20));
    if !p && !u {
        if !bit(w, 22) || wb || bits(w, 7, 6) != 0 || !bit(w, 4) {
            return undef();
        }
        let rt = bits(w, 15, 12) as u8;
        let rt2 = bits(w, 19, 16) as u8;
        let vm = vreg(dbl, bits(w, 3, 0), bit(w, 5));
        if rt == 15 || rt2 == 15 || (!dbl && vm == 31) || (l && rt == rt2) {
            return undef();
        }
        return Insn {
            rd: rt,
            ra: rt2,
            rm: vm,
            op: l as u8,
            flags: fl,
            ..Insn::new(Kind::VmovCore2)
        };
    }
    if p && !wb {
        return Insn {
            rd: vd,
            rn,
            imm: imm8 * 4,
            flags: fl | if u { F_U } else { 0 },
            ..Insn::new(if l { Kind::Vldr } else { Kind::Vstr })
        };
    }
    if p && u {
        return undef();
    }
    let count = if dbl { imm8 / 2 } else { imm8 };
    if count == 0 || vd as u32 + count > 32 || (dbl && count > 16) || (wb && rn == 15) {
        return undef();
    }
    Insn {
        rd: vd,
        rn,
        imm: count,
        op: if p { lsm::DB } else { lsm::IA },
        flags: fl | if wb { F_W } else { 0 },
        ..Insn::new(if l { Kind::Vldm } else { Kind::Vstm })
    }
}

/// Advanced SIMD data processing (A32 layout, `1111001U ...`). Only the
/// floating-point VADD/VSUB/VMUL are implemented, which is all the guest
/// uses.
pub fn decode_neon(w: u32) -> Insn {
    let u = bit(w, 24);
    let op = match (u, bit(w, 23), bits(w, 21, 20), bits(w, 11, 8), bit(w, 4)) {
        (false, false, 0b00, 0b1101, false) => 0,
        (false, false, 0b10, 0b1101, false) => 1,
        (true, false, 0b00, 0b1101, true) => 2,
        _ => return undef(),
    };
    let q = bit(w, 6);
    let d = vreg(true, bits(w, 15, 12), bit(w, 22));
    let n = vreg(true, bits(w, 19, 16), bit(w, 7));
    let m = vreg(true, bits(w, 3, 0), bit(w, 5));
    if q && (d | n | m) & 1 != 0 {
        return undef();
    }
    Insn {
        op,
        rd: d,
        rn: n,
        rm: m,
        imm: if q { 2 } else { 1 },
        ..Insn::new(Kind::NeonF32)
    }
}

// FPSCR bits.
const IOC: u32 = 1 << 0;
const DZC: u32 = 1 << 1;
const OFC: u32 = 1 << 2;
const UFC: u32 = 1 << 3;
const IXC: u32 = 1 << 4;
const IDC: u32 = 1 << 7;
const FZ: u32 = 1 << 24;
const DN: u32 = 1 << 25;
const RMODE_SHIFT: u32 = 22;
const RN: u32 = 0;
const RP: u32 = 1;
const RM: u32 = 2;
const RZ: u32 = 3;

/// The "standard FPSCR value" used by Advanced SIMD operations.
const NEON_CTL: u32 = FZ | DN;

/// FPSCR bits that VMSR can set (the trap enable bits are RAZ/WI).
pub const FPSCR_MASK: u32 = 0xfff7_009f;

/// A binary floating-point format.
#[derive(Clone, Copy)]
struct Fmt {
    /// Explicit mantissa bits.
    mbits: u32,
    /// Exponent bits.
    ebits: u32,
}
const F32: Fmt = Fmt {
    mbits: 23,
    ebits: 8,
};
const F64: Fmt = Fmt {
    mbits: 52,
    ebits: 11,
};

impl Fmt {
    fn bias(self) -> i32 {
        (1 << (self.ebits - 1)) - 1
    }
    fn sign(self) -> u64 {
        1 << (self.mbits + self.ebits)
    }
    fn mmask(self) -> u64 {
        (1 << self.mbits) - 1
    }
    fn emask(self) -> u64 {
        (1 << self.ebits) - 1
    }
    fn exp_field(self, x: u64) -> u64 {
        (x >> self.mbits) & self.emask()
    }
    fn is_nan(self, x: u64) -> bool {
        self.exp_field(x) == self.emask() && x & self.mmask() != 0
    }
    fn quiet_bit(self) -> u64 {
        1 << (self.mbits - 1)
    }
    fn is_snan(self, x: u64) -> bool {
        self.is_nan(x) && x & self.quiet_bit() == 0
    }
    fn default_nan(self) -> u64 {
        (self.emask() << self.mbits) | self.quiet_bit()
    }
    fn inf(self, sign: bool) -> u64 {
        (if sign { self.sign() } else { 0 }) | self.emask() << self.mbits
    }
    fn max(self, sign: bool) -> u64 {
        (if sign { self.sign() } else { 0 }) | (self.emask() - 1) << self.mbits | self.mmask()
    }
    fn zero(self, sign: bool) -> u64 {
        if sign {
            self.sign()
        } else {
            0
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Zero,
    Finite,
    Inf,
    Nan,
}

/// An unpacked operand. For finite values, value = sig / 2^63 * 2^exp with
/// bit 63 of `sig` set.
#[derive(Clone, Copy)]
struct Unpacked {
    class: Class,
    sign: bool,
    exp: i32,
    sig: u64,
}

/// FPUnpack: also flushes denormal inputs to zero if FZ is set.
fn unpack(x: u64, f: Fmt, ctl: u32, flags: &mut u32) -> Unpacked {
    let sign = x & f.sign() != 0;
    let e = f.exp_field(x);
    let m = x & f.mmask();
    let (class, exp, sig) = if e == f.emask() {
        (if m == 0 { Class::Inf } else { Class::Nan }, 0, 0)
    } else if e == 0 {
        if m == 0 {
            (Class::Zero, 0, 0)
        } else if ctl & FZ != 0 {
            *flags |= IDC;
            (Class::Zero, 0, 0)
        } else {
            let lz = m.leading_zeros();
            (
                Class::Finite,
                1 - f.bias() - (lz as i32 - (63 - f.mbits as i32)),
                m << lz,
            )
        }
    } else {
        (
            Class::Finite,
            e as i32 - f.bias(),
            (m | 1 << f.mbits) << (63 - f.mbits),
        )
    };
    Unpacked {
        class,
        sign,
        exp,
        sig,
    }
}

fn shift_right_jam(x: u64, n: i32) -> u64 {
    if n <= 0 {
        x
    } else if n >= 64 {
        (x != 0) as u64
    } else {
        x >> n | (x << (64 - n) != 0) as u64
    }
}

/// Normalize a 128-bit significand whose value is `s / 2^point * 2^exp` to
/// the (exp, sig) form, keeping a sticky bit.
fn norm128(s: u128, point: i32, exp: i32) -> (i32, u64) {
    let lz = s.leading_zeros() as i32;
    let s = s << lz;
    let sig = (s >> 64) as u64 | ((s as u64) != 0) as u64;
    (exp + (127 - lz) - point, sig)
}

/// FPRound: round (sign, exp, sig) to the format according to `ctl`.
fn round_pack(sign: bool, mut exp: i32, mut sig: u64, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    let emin = 1 - f.bias();
    let p = f.mbits + 1;
    let mut tiny = false;
    if exp < emin {
        if ctl & FZ != 0 {
            *flags |= UFC;
            return f.zero(sign);
        }
        sig = shift_right_jam(sig, emin - exp);
        exp = emin;
        tiny = true;
    }
    let rbits = 64 - p;
    let mut m = sig >> rbits;
    let rem = sig & ((1 << rbits) - 1);
    let half = 1 << (rbits - 1);
    let inexact = rem != 0;
    let inc = match (ctl >> RMODE_SHIFT) & 3 {
        RN => rem > half || (rem == half && m & 1 != 0),
        RP => !sign && inexact,
        RM => sign && inexact,
        _ => false,
    };
    if inc {
        m += 1;
        if m == 1 << p {
            m >>= 1;
            exp += 1;
        }
    }
    if exp > f.bias() {
        *flags |= OFC | IXC;
        let to_inf = match (ctl >> RMODE_SHIFT) & 3 {
            RN => true,
            RP => !sign,
            RM => sign,
            _ => false,
        };
        return if to_inf { f.inf(sign) } else { f.max(sign) };
    }
    if inexact {
        *flags |= IXC;
        if tiny {
            *flags |= UFC;
        }
    }
    let be = if m >> (p - 1) != 0 {
        (exp + f.bias()) as u64
    } else {
        0
    };
    f.zero(sign) | be << f.mbits | (m & f.mmask())
}

/// FPProcessNaN.
fn process_nan(x: u64, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    if f.is_snan(x) {
        *flags |= IOC;
    }
    if ctl & DN != 0 {
        f.default_nan()
    } else {
        x | f.quiet_bit()
    }
}

/// FPProcessNaNs: returns the NaN result if either operand is a NaN.
fn process_nans(a: u64, b: u64, f: Fmt, ctl: u32, flags: &mut u32) -> Option<u64> {
    if f.is_snan(a) {
        Some(process_nan(a, f, ctl, flags))
    } else if f.is_snan(b) {
        Some(process_nan(b, f, ctl, flags))
    } else if f.is_nan(a) {
        Some(process_nan(a, f, ctl, flags))
    } else if f.is_nan(b) {
        Some(process_nan(b, f, ctl, flags))
    } else {
        None
    }
}

fn invalid(f: Fmt, flags: &mut u32) -> u64 {
    *flags |= IOC;
    f.default_nan()
}

fn add_slow(a: u64, b: u64, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    // Like FPUnpack, flush denormals (setting IDC) before processing NaNs.
    let x = unpack(a, f, ctl, flags);
    let y = unpack(b, f, ctl, flags);
    if let Some(r) = process_nans(a, b, f, ctl, flags) {
        return r;
    }
    let rm_mode = (ctl >> RMODE_SHIFT) & 3 == RM;
    match (x.class, y.class) {
        (Class::Inf, Class::Inf) if x.sign != y.sign => return invalid(f, flags),
        (Class::Inf, _) => return f.inf(x.sign),
        (_, Class::Inf) => return f.inf(y.sign),
        (Class::Zero, Class::Zero) => {
            return f.zero(if x.sign == y.sign { x.sign } else { rm_mode });
        }
        (Class::Zero, _) => return round_pack(y.sign, y.exp, y.sig, f, ctl, flags),
        (_, Class::Zero) => return round_pack(x.sign, x.exp, x.sig, f, ctl, flags),
        _ => (),
    }
    let (x, y) = if (x.exp, x.sig) >= (y.exp, y.sig) {
        (x, y)
    } else {
        (y, x)
    };
    let big = (x.sig as u128) << 62;
    let d = (x.exp - y.exp) as u32;
    let small = if d >= 127 {
        1
    } else {
        let s = (y.sig as u128) << 62;
        s >> d | ((s << (128 - d.max(1))) != 0 && d != 0) as u128
    };
    let (sign, s) = if x.sign == y.sign {
        (x.sign, big + small)
    } else {
        (x.sign, big - small)
    };
    if s == 0 {
        return f.zero(rm_mode);
    }
    let (exp, sig) = norm128(s, 125, x.exp);
    round_pack(sign, exp, sig, f, ctl, flags)
}

fn mul_slow(a: u64, b: u64, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    // Like FPUnpack, flush denormals (setting IDC) before processing NaNs.
    let x = unpack(a, f, ctl, flags);
    let y = unpack(b, f, ctl, flags);
    if let Some(r) = process_nans(a, b, f, ctl, flags) {
        return r;
    }
    let sign = x.sign != y.sign;
    match (x.class, y.class) {
        (Class::Inf, Class::Zero) | (Class::Zero, Class::Inf) => invalid(f, flags),
        (Class::Inf, _) | (_, Class::Inf) => f.inf(sign),
        (Class::Zero, _) | (_, Class::Zero) => f.zero(sign),
        _ => {
            let (exp, sig) = norm128(x.sig as u128 * y.sig as u128, 126, x.exp + y.exp);
            round_pack(sign, exp, sig, f, ctl, flags)
        }
    }
}

fn div_slow(a: u64, b: u64, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    // Like FPUnpack, flush denormals (setting IDC) before processing NaNs.
    let x = unpack(a, f, ctl, flags);
    let y = unpack(b, f, ctl, flags);
    if let Some(r) = process_nans(a, b, f, ctl, flags) {
        return r;
    }
    let sign = x.sign != y.sign;
    match (x.class, y.class) {
        (Class::Inf, Class::Inf) | (Class::Zero, Class::Zero) => invalid(f, flags),
        (Class::Inf, _) | (_, Class::Zero) => {
            if y.class == Class::Zero {
                *flags |= DZC;
            }
            f.inf(sign)
        }
        (Class::Zero, _) | (_, Class::Inf) => f.zero(sign),
        _ => {
            let n = (x.sig as u128) << 64;
            let q = n / y.sig as u128;
            let r = n % y.sig as u128;
            let (exp, sig) = norm128(q, 64, x.exp - y.exp);
            round_pack(sign, exp, sig | (r != 0) as u64, f, ctl, flags)
        }
    }
}

fn sqrt_slow(a: u64, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    if f.is_nan(a) {
        return process_nan(a, f, ctl, flags);
    }
    let x = unpack(a, f, ctl, flags);
    match x.class {
        Class::Zero => f.zero(x.sign),
        _ if x.sign => invalid(f, flags),
        Class::Inf => f.inf(false),
        _ => {
            let odd = x.exp & 1 != 0;
            let m = (x.sig as u128) << if odd { 64 } else { 63 };
            let exp = (x.exp - odd as i32) / 2;
            // Integer square root: estimate, then correct.
            let mut r = ((m as f64).sqrt() as u128).max(1);
            for _ in 0..4 {
                r = (r + m / r) / 2;
            }
            while r * r > m {
                r -= 1;
            }
            while (r + 1) * (r + 1) <= m {
                r += 1;
            }
            let sticky = (r * r != m) as u64;
            round_pack(false, exp, r as u64 | sticky, f, ctl, flags)
        }
    }
}

/// FPCompare: returns NZCV in bits 3:0.
fn compare(a: u64, b: u64, f: Fmt, quiet_nan_exc: bool, ctl: u32, flags: &mut u32) -> u32 {
    let x = to_host(a, f, ctl, flags);
    let y = to_host(b, f, ctl, flags);
    if f.is_nan(a) || f.is_nan(b) {
        if quiet_nan_exc || f.is_snan(a) || f.is_snan(b) {
            *flags |= IOC;
        }
        return 0b0011;
    }
    if x == y {
        0b0110
    } else if x < y {
        0b1000
    } else {
        0b0010
    }
}

/// Converts a non-NaN value to a host double (exactly), applying input
/// flushing.
fn to_host(a: u64, f: Fmt, ctl: u32, flags: &mut u32) -> f64 {
    let u = unpack(a, f, ctl, flags);
    match u.class {
        Class::Zero => 0.0,
        Class::Inf if u.sign => f64::NEG_INFINITY,
        Class::Inf => f64::INFINITY,
        _ if f.mbits == 23 => f32::from_bits(a as u32) as f64,
        _ => f64::from_bits(a),
    }
}

/// FPToFixed with zero fraction bits, to a 32-bit integer.
fn to_int(a: u64, f: Fmt, signed: bool, round_zero: bool, ctl: u32, flags: &mut u32) -> u32 {
    if f.is_nan(a) {
        *flags |= IOC;
        return 0;
    }
    let x = unpack(a, f, ctl, flags);
    let saturate = |flags: &mut u32| {
        *flags |= IOC;
        match (signed, x.sign) {
            (true, true) => i32::MIN as u32,
            (true, false) => i32::MAX as u32,
            (false, true) => 0,
            (false, false) => u32::MAX,
        }
    };
    match x.class {
        Class::Zero => return 0,
        Class::Inf => return saturate(flags),
        _ => (),
    }
    if x.exp >= 63 {
        return saturate(flags);
    }
    let (ip, frac) = if x.exp >= 0 {
        (
            x.sig >> (63 - x.exp),
            if x.exp == 63 { 0 } else { x.sig << (x.exp + 1) },
        )
    } else {
        (0, shift_right_jam(x.sig, -x.exp - 1))
    };
    let mode = if round_zero {
        RZ
    } else {
        (ctl >> RMODE_SHIFT) & 3
    };
    let half = 1 << 63;
    let inc = match mode {
        RN => frac > half || (frac == half && ip & 1 != 0),
        RP => !x.sign && frac != 0,
        RM => x.sign && frac != 0,
        _ => false,
    };
    let ip = ip + inc as u64;
    let result = if signed {
        if x.sign {
            if ip > 1 << 31 {
                return saturate(flags);
            }
            (ip as u32).wrapping_neg()
        } else {
            if ip > i32::MAX as u64 {
                return saturate(flags);
            }
            ip as u32
        }
    } else {
        if (x.sign && ip != 0) || ip > u32::MAX as u64 {
            return saturate(flags);
        }
        ip as u32
    };
    if frac != 0 {
        *flags |= IXC;
    }
    result
}

fn from_int(v: u32, signed: bool, f: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    let sign = signed && (v as i32) < 0;
    let mag = if sign { (v as i32).unsigned_abs() } else { v } as u64;
    if mag == 0 {
        return 0;
    }
    let lz = mag.leading_zeros();
    round_pack(sign, 63 - lz as i32, mag << lz, f, ctl, flags)
}

/// FPConvert between formats.
fn convert(a: u64, from: Fmt, to: Fmt, ctl: u32, flags: &mut u32) -> u64 {
    if from.is_nan(a) {
        if from.is_snan(a) {
            *flags |= IOC;
        }
        if ctl & DN != 0 {
            return to.default_nan();
        }
        let sign = a & from.sign() != 0;
        let m = a & from.mmask();
        let m = if to.mbits > from.mbits {
            m << (to.mbits - from.mbits)
        } else {
            m >> (from.mbits - to.mbits)
        };
        return to.inf(sign) | m | to.quiet_bit();
    }
    let x = unpack(a, from, ctl, flags);
    match x.class {
        Class::Zero => to.zero(x.sign),
        Class::Inf => to.inf(x.sign),
        _ => round_pack(x.sign, x.exp, x.sig, to, ctl, flags),
    }
}

// Fast paths. These apply when rounding to nearest and both the inputs and
// the result are normal numbers or exact zeros, so the host result is the
// correctly rounded one and FZ/DN do not matter. The only flag that can be
// raised then is inexact, which is computed with error-free transformations.

fn is_plain32(x: f32) -> bool {
    x.is_normal() || x == 0.0
}
fn is_plain64(x: f64) -> bool {
    x.is_normal() || x == 0.0
}

/// The error of `a + b` as computed in double precision (TwoSum).
fn two_sum_err(a: f64, b: f64, s: f64) -> f64 {
    let bb = s - a;
    (a - (s - bb)) + (b - bb)
}

fn add32(a: u32, b: u32, ctl: u32, flags: &mut u32) -> u32 {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if (ctl >> RMODE_SHIFT) & 3 == RN && is_plain32(x) && is_plain32(y) {
        // Single-precision results are correctly rounded from double ones.
        let s = x as f64 + y as f64;
        let r = s as f32;
        if r.is_normal() {
            if r as f64 != s || two_sum_err(x as f64, y as f64, s) != 0.0 {
                *flags |= IXC;
            }
            return r.to_bits();
        }
    }
    add_slow(a as u64, b as u64, F32, ctl, flags) as u32
}

fn mul32(a: u32, b: u32, ctl: u32, flags: &mut u32) -> u32 {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if (ctl >> RMODE_SHIFT) & 3 == RN && is_plain32(x) && is_plain32(y) {
        // The double-precision product is exact.
        let p = x as f64 * y as f64;
        let r = p as f32;
        if r.is_normal() || (r == 0.0 && p == 0.0) {
            if r as f64 != p {
                *flags |= IXC;
            }
            return r.to_bits();
        }
    }
    mul_slow(a as u64, b as u64, F32, ctl, flags) as u32
}

fn div32(a: u32, b: u32, ctl: u32, flags: &mut u32) -> u32 {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if (ctl >> RMODE_SHIFT) & 3 == RN && is_plain32(x) && y.is_normal() {
        let q = x as f64 / y as f64;
        let r = q as f32;
        if r.is_normal() || x == 0.0 {
            // The double quotient is inexact iff q * y != x exactly; the
            // product of a 53-bit and a 24-bit significand fits in f64 only
            // approximately, so check the residual with an FMA instead.
            if r as f64 != q || (-q).mul_add(y as f64, x as f64) != 0.0 {
                *flags |= IXC;
            }
            return r.to_bits();
        }
    }
    div_slow(a as u64, b as u64, F32, ctl, flags) as u32
}

fn add64(a: u64, b: u64, ctl: u32, flags: &mut u32) -> u64 {
    let (x, y) = (f64::from_bits(a), f64::from_bits(b));
    if (ctl >> RMODE_SHIFT) & 3 == RN && is_plain64(x) && is_plain64(y) {
        let s = x + y;
        if s.is_normal() {
            if two_sum_err(x, y, s) != 0.0 {
                *flags |= IXC;
            }
            return s.to_bits();
        }
    }
    add_slow(a, b, F64, ctl, flags)
}

fn mul64(a: u64, b: u64, ctl: u32, flags: &mut u32) -> u64 {
    let (x, y) = (f64::from_bits(a), f64::from_bits(b));
    // The FMA residual is only exact if the product doesn't come near the
    // subnormal range.
    if (ctl >> RMODE_SHIFT) & 3 == RN && is_plain64(x) && is_plain64(y) {
        let p = x * y;
        if p.is_finite() && p.abs() >= f64::MIN_POSITIVE * 2f64.powi(54) {
            if x.mul_add(y, -p) != 0.0 {
                *flags |= IXC;
            }
            return p.to_bits();
        }
        if p == 0.0 && (x == 0.0 || y == 0.0) {
            return p.to_bits();
        }
    }
    mul_slow(a, b, F64, ctl, flags)
}

fn div64(a: u64, b: u64, ctl: u32, flags: &mut u32) -> u64 {
    let (x, y) = (f64::from_bits(a), f64::from_bits(b));
    if (ctl >> RMODE_SHIFT) & 3 == RN && is_plain64(x) && y.is_normal() {
        let q = x / y;
        // The residual is exact if both x and q are well above the subnormal
        // range.
        let big = f64::MIN_POSITIVE * 2f64.powi(54);
        if q.is_finite() && ((q.abs() >= big && x.abs() >= big) || x == 0.0) {
            if (-q).mul_add(y, x) != 0.0 {
                *flags |= IXC;
            }
            return q.to_bits();
        }
    }
    div_slow(a, b, F64, ctl, flags)
}

/// FPSub: NaN operands propagate without being negated.
fn sub32(a: u32, b: u32, ctl: u32, flags: &mut u32) -> u32 {
    if F32.is_nan(a as u64) || F32.is_nan(b as u64) {
        return add_slow(a as u64, b as u64, F32, ctl, flags) as u32;
    }
    add32(a, neg32(b), ctl, flags)
}

fn sub64(a: u64, b: u64, ctl: u32, flags: &mut u32) -> u64 {
    if F64.is_nan(a) || F64.is_nan(b) {
        return add_slow(a, b, F64, ctl, flags);
    }
    add64(a, neg64(b), ctl, flags)
}

fn neg32(a: u32) -> u32 {
    a ^ 0x8000_0000
}
fn neg64(a: u64) -> u64 {
    a ^ (1 << 63)
}

impl Cpu {
    fn s(&self, i: u8) -> u32 {
        self.ext[(i & 31) as usize]
    }
    fn set_s(&mut self, i: u8, v: u32) {
        self.ext[(i & 31) as usize] = v;
    }
    fn d(&self, i: u8) -> u64 {
        let i = (i & 31) as usize * 2;
        self.ext[i] as u64 | (self.ext[i + 1] as u64) << 32
    }
    fn set_d(&mut self, i: u8, v: u64) {
        let i = (i & 31) as usize * 2;
        self.ext[i] = v as u32;
        self.ext[i + 1] = (v >> 32) as u32;
    }

    /// Executes [Kind::Vfp] instructions.
    pub(crate) fn exec_vfp(&mut self, i: &Insn) {
        let ctl = self.fpscr;
        let mut fl = 0;
        let flags = &mut fl;
        if !i.has(F_DBL) {
            let (d, n, m) = (self.s(i.rd), self.s(i.rn), self.s(i.rm));
            let r = match i.op {
                vfp_op::MLA => add32(d, mul32(n, m, ctl, flags), ctl, flags),
                vfp_op::MLS => add32(d, neg32(mul32(n, m, ctl, flags)), ctl, flags),
                vfp_op::NMLS => add32(neg32(d), mul32(n, m, ctl, flags), ctl, flags),
                vfp_op::NMLA => add32(neg32(d), neg32(mul32(n, m, ctl, flags)), ctl, flags),
                vfp_op::MUL => mul32(n, m, ctl, flags),
                vfp_op::NMUL => neg32(mul32(n, m, ctl, flags)),
                vfp_op::ADD => add32(n, m, ctl, flags),
                vfp_op::SUB => sub32(n, m, ctl, flags),
                vfp_op::DIV => div32(n, m, ctl, flags),
                vfp_op::MOV_IMM => i.imm,
                vfp_op::MOV => m,
                vfp_op::ABS => m & 0x7fff_ffff,
                vfp_op::NEG => neg32(m),
                vfp_op::SQRT => sqrt_slow(m as u64, F32, ctl, flags) as u32,
                vfp_op::CMP => {
                    let m = if i.sh != 0 { 0 } else { m };
                    let nzcv = compare(d as u64, m as u64, F32, i.amt != 0, ctl, flags);
                    self.fpscr = (self.fpscr & 0x0fff_ffff) | nzcv << 28 | fl;
                    return;
                }
                vfp_op::CVT_FP => {
                    let r = convert(m as u64, F32, F64, ctl, flags);
                    self.set_d(i.rd, r);
                    self.fpscr |= fl;
                    return;
                }
                vfp_op::CVT_FROM_INT => from_int(m, i.amt != 0, F32, ctl, flags) as u32,
                vfp_op::CVT_TO_INT => to_int(m as u64, F32, i.amt != 0, i.sh != 0, ctl, flags),
                _ => unreachable!(),
            };
            self.set_s(i.rd, r);
        } else {
            let (m, rm_s) = (self.d(i.rm), self.s(i.rm));
            let r = match i.op {
                vfp_op::CVT_FP => {
                    let r = convert(m, F64, F32, ctl, flags) as u32;
                    self.set_s(i.rd, r);
                    self.fpscr |= fl;
                    return;
                }
                vfp_op::CVT_FROM_INT => from_int(rm_s, i.amt != 0, F64, ctl, flags),
                vfp_op::CVT_TO_INT => {
                    let r = to_int(m, F64, i.amt != 0, i.sh != 0, ctl, flags);
                    self.set_s(i.rd, r);
                    self.fpscr |= fl;
                    return;
                }
                _ => {
                    let (d, n) = (self.d(i.rd), self.d(i.rn));
                    match i.op {
                        vfp_op::MLA => add64(d, mul64(n, m, ctl, flags), ctl, flags),
                        vfp_op::MLS => add64(d, neg64(mul64(n, m, ctl, flags)), ctl, flags),
                        vfp_op::NMLS => add64(neg64(d), mul64(n, m, ctl, flags), ctl, flags),
                        vfp_op::NMLA => add64(neg64(d), neg64(mul64(n, m, ctl, flags)), ctl, flags),
                        vfp_op::MUL => mul64(n, m, ctl, flags),
                        vfp_op::NMUL => neg64(mul64(n, m, ctl, flags)),
                        vfp_op::ADD => add64(n, m, ctl, flags),
                        vfp_op::SUB => sub64(n, m, ctl, flags),
                        vfp_op::DIV => div64(n, m, ctl, flags),
                        vfp_op::MOV_IMM => (i.imm as u64) << 32,
                        vfp_op::MOV => m,
                        vfp_op::ABS => m & !(1 << 63),
                        vfp_op::NEG => neg64(m),
                        vfp_op::SQRT => sqrt_slow(m, F64, ctl, flags),
                        vfp_op::CMP => {
                            let m = if i.sh != 0 { 0 } else { m };
                            let nzcv = compare(d, m, F64, i.amt != 0, ctl, flags);
                            self.fpscr = (self.fpscr & 0x0fff_ffff) | nzcv << 28 | fl;
                            return;
                        }
                        _ => unreachable!(),
                    }
                }
            };
            self.set_d(i.rd, r);
        }
        self.fpscr |= fl;
    }

    /// Executes [Kind::NeonF32].
    pub(crate) fn exec_neon_f32(&mut self, i: &Insn) {
        let mut fl = 0;
        for k in 0..(i.imm as usize * 2) {
            let a = self.ext[(i.rn as usize * 2 + k) & 63];
            let b = self.ext[(i.rm as usize * 2 + k) & 63];
            let r = match i.op {
                0 => add32(a, b, NEON_CTL, &mut fl),
                1 => sub32(a, b, NEON_CTL, &mut fl),
                _ => mul32(a, b, NEON_CTL, &mut fl),
            };
            self.ext[(i.rd as usize * 2 + k) & 63] = r;
        }
        self.fpscr |= fl;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow_add32(a: f32, b: f32) -> (f32, u32) {
        let mut fl = 0;
        let r = add_slow(a.to_bits() as u64, b.to_bits() as u64, F32, 0, &mut fl);
        (f32::from_bits(r as u32), fl)
    }

    #[test]
    fn slow_paths_match_host() {
        // Simple deterministic sweep comparing the integer implementation
        // with host arithmetic in round-to-nearest.
        let mut x: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..100_000 {
            let (a, b) = (next(), next());
            let (fa, fb) = (f64::from_bits(a), f64::from_bits(b));
            if fa.is_nan() || fb.is_nan() {
                continue;
            }
            let mut fl = 0;
            let check = |r: u64, h: f64| {
                assert!(r == h.to_bits() || (h.is_nan() && f64::from_bits(r).is_nan()))
            };
            check(add_slow(a, b, F64, 0, &mut fl), fa + fb);
            check(mul_slow(a, b, F64, 0, &mut fl), fa * fb);
            check(div_slow(a, b, F64, 0, &mut fl), fa / fb);
            check(sqrt_slow(a, F64, 0, &mut fl), fa.sqrt());
            let (sa, sb) = (a as u32, b as u32);
            let (ga, gb) = (f32::from_bits(sa), f32::from_bits(sb));
            if !ga.is_nan() && !gb.is_nan() {
                let (r, _) = slow_add32(ga, gb);
                assert!(r.to_bits() == (ga + gb).to_bits() || r.is_nan());
                let mut f1 = 0;
                let mut f2 = 0;
                assert_eq!(
                    add32(sa, sb, 0, &mut f1),
                    add_slow(sa as u64, sb as u64, F32, 0, &mut f2) as u32
                );
                assert_eq!(f1, f2, "{ga} + {gb}");
                let (mut f1, mut f2) = (0, 0);
                assert_eq!(
                    mul32(sa, sb, 0, &mut f1),
                    mul_slow(sa as u64, sb as u64, F32, 0, &mut f2) as u32
                );
                assert_eq!(f1, f2, "{ga} * {gb}");
                let (mut f1, mut f2) = (0, 0);
                assert_eq!(
                    div32(sa, sb, 0, &mut f1),
                    div_slow(sa as u64, sb as u64, F32, 0, &mut f2) as u32
                );
                assert_eq!(f1, f2, "{ga} / {gb}");
            }
            let (mut f1, mut f2) = (0, 0);
            assert_eq!(add64(a, b, 0, &mut f1), add_slow(a, b, F64, 0, &mut f2));
            assert_eq!(f1, f2);
            let (mut f1, mut f2) = (0, 0);
            assert_eq!(mul64(a, b, 0, &mut f1), mul_slow(a, b, F64, 0, &mut f2));
            assert_eq!(f1, f2);
            let (mut f1, mut f2) = (0, 0);
            assert_eq!(div64(a, b, 0, &mut f1), div_slow(a, b, F64, 0, &mut f2));
            assert_eq!(f1, f2, "{fa:e} / {fb:e} = {:e}", fa / fb);
        }
    }
}
