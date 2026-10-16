# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Generates differential test vectors for the interpreter using unicorn.

Usage (from this directory):

    uv run --with unicorn==2.1.4 --with keystone-engine==0.9.2 python -I \\
        gen_vectors.py vectors.txt

The cases come from the assembly templates below. They contain no code
from the game, so the vectors can be published. (`survey.py` lists the
encodings a guest binary uses, to check that the templates cover them.)

Each case runs a short instruction sequence in unicorn (Cortex-A8, user
mode) from an initial state derived from a seed, and records the end state.
`vectors.rs` recreates the initial state from the seed, runs the
interpreter one instruction at a time, and compares. `initial_state` and
`base_memory` must stay in sync with `vectors.rs`.

Line format:

    <A|T> <seed> <code> <steps> <cpsr> <fpscr> [rN=x ...] : <status>
        [rN=x ...] c=<cpsr> f=<fpscr> [eN=x ...] [m<addr>=<bytes> ...]

The part before the colon is the input: the instruction set, the seed, the
code bytes, the number of instructions to run, the initial CPSR and FPSCR,
and register overrides. After the colon: the status (-1 for normal
completion, -2 memory error, -3 undefined instruction, -4 BKPT, or an SVC
immediate), registers that changed, CPSR (masked to NZCVQ, IT, GE, T),
FPSCR, changed extension register words, and changed memory. For memory
errors only the PC is recorded.

Known differences from unicorn, handled here in favor of dynarmic's
behavior, which the interpreter replaces:
- After BKPT and undefined instructions, unicorn leaves the PC at the
  instruction; dynarmic puts it after. The expected PC is adjusted.
- Exclusive accesses to unaligned addresses fault in unicorn; dynarmic
  doesn't check alignment. Templates align the address; any remaining
  alignment faults are skipped.
- An interworking branch to an ARM address with bit 1 set is UNPREDICTABLE:
  unicorn keeps bit 1, dynarmic clears it. Such cases are skipped.
- `mcr p15, 0, rX, c7, c10, 4` (CP15DSB) and other CP15 operations except
  CP15DMB and TPIDRURO reads are undefined under touchHLE's dynarmic
  configuration but work in unicorn, so they aren't generated.
"""

import random
import struct
import sys

from keystone import KS_ARCH_ARM, KS_MODE_ARM, KS_MODE_THUMB, Ks, KsError
from unicorn import UC_ARCH_ARM, UC_HOOK_INTR, UC_MODE_ARM, Uc, UcError
from unicorn import unicorn_const as ucc
from unicorn.arm_const import *

CODE = 0x1000
WIN = 0x2000
MEM_END = 0x20000
# R13 and R14 aren't numbered contiguously with R0-R12 in unicorn.
GPRS = [UC_ARM_REG_R0 + i for i in range(13)] + [UC_ARM_REG_SP, UC_ARM_REG_LR]


def xorshift(s):
    s ^= (s << 13) & 0xFFFFFFFF
    s ^= s >> 17
    s ^= (s << 5) & 0xFFFFFFFF
    return s


def base_memory():
    """Fixed contents of [WIN, MEM_END), the same for every case."""
    out = bytearray()
    s = 0x12345678
    for _ in range((MEM_END - WIN) // 4):
        s = xorshift(s)
        out += struct.pack("<I", s)
    return bytes(out)


BASE_MEM = base_memory()


class Rng:
    def __init__(self, seed):
        self.s = seed or 1

    def next(self):
        self.s = xorshift(self.s)
        return self.s


SPECIAL = [0, 1, 0x7FFFFFFF, 0x80000000, 0xFFFFFFFF, 0x8000, 0xFFFF]
F32S = [0x00000000, 0x80000000, 0x3F800000, 0xBF800000, 0x7F800000, 0xFF800000,
        0x7FC00000, 0x7F800001, 0xFFC00001, 0x00000001, 0x807FFFFF, 0x00800000,
        0x7F7FFFFF, 0x4B000000, 0x4F000000, 0xCF000000, 0x3F000000, 0x4EFFFFFF]
F64S = [0x0000000000000000, 0x8000000000000000, 0x3FF0000000000000,
        0x7FF0000000000000, 0xFFF0000000000000, 0x7FF8000000000000,
        0x7FF0000000000001, 0x0000000000000001, 0x0010000000000000,
        0x7FEFFFFFFFFFFFFF, 0x41E0000000000000, 0xC1E0000000000000,
        0x41EFFFFFFFE00000, 0x3FE0000000000000, 0x36A0000000000000,
        0x47EFFFFFE0000000, 0x3810000000000000]


def f32bits(v):
    return struct.unpack("<I", struct.pack("<f", v))[0]


def f64bits(v):
    return struct.unpack("<Q", struct.pack("<d", v))[0]


def initial_state(seed):
    """Registers, extension registers and TPIDRURO for a seed."""
    r = Rng(seed)

    def val():
        c = r.next() % 8
        if c < 3:
            return 0x4000 + r.next() % 0x18000
        if c < 5:
            return ((r.next() % 512) - 256) & 0xFFFFFFFF
        if c == 5:
            return SPECIAL[r.next() % len(SPECIAL)]
        return r.next()

    regs = [val() for _ in range(16)]
    regs[13] = 0x10000 + (r.next() % 0x1000) * 4
    regs[15] = CODE
    ext = []
    for _ in range(32):
        c = r.next() % 6
        if c == 0:
            ext += [r.next(), r.next()]
        elif c == 1:
            ext += [F32S[r.next() % len(F32S)], F32S[r.next() % len(F32S)]]
        elif c == 2:
            d = F64S[r.next() % len(F64S)]
            ext += [d & 0xFFFFFFFF, d >> 32]
        elif c == 3:
            a = ((r.next() % 2001) - 1000) / 16.0
            b = ((r.next() % 2001) - 1000) / 16.0
            ext += [f32bits(a), f32bits(b)]
        elif c == 4:
            d = f64bits(((r.next() % 20001) - 10000) / 64.0)
            ext += [d & 0xFFFFFFFF, d >> 32]
        else:
            for _ in range(2):
                sign = r.next() & 0x80000000
                exp = 117 + r.next() % 20
                ext.append(sign | exp << 23 | (r.next() & 0x7FFFFF))
    tpidruro = r.next()
    return regs, ext, tpidruro


def run_unicorn(thumb, code, code_len, steps, regs, ext, cpsr, fpscr, tpidruro):
    uc = Uc(UC_ARCH_ARM, UC_MODE_ARM)
    uc.ctl_set_cpu_model(UC_CPU_ARM_CORTEX_A8)
    uc.mem_map(CODE, MEM_END - CODE)
    uc.mem_write(CODE, code)
    uc.mem_write(WIN, BASE_MEM)
    # Enable VFP/NEON.
    uc.reg_write(UC_ARM_REG_C1_C0_2, uc.reg_read(UC_ARM_REG_C1_C0_2) | (0xF << 20))
    uc.reg_write(UC_ARM_REG_FPEXC, 0x40000000)
    uc.reg_write(UC_ARM_REG_CPSR, cpsr)
    for i in range(15):
        uc.reg_write(GPRS[i], regs[i])
    for i in range(32):
        uc.reg_write(UC_ARM_REG_D0 + i, ext[2 * i] | ext[2 * i + 1] << 32)
    uc.reg_write(UC_ARM_REG_FPSCR, fpscr)
    uc.reg_write(UC_ARM_REG_C13_C0_3, tpidruro)
    interrupts = []

    def hook(uc, intno, _):
        interrupts.append(intno)
        uc.emu_stop()

    uc.hook_add(UC_HOOK_INTR, hook)
    err = None
    try:
        if steps == 1:
            uc.emu_start(CODE | thumb, MEM_END, count=1)
        else:
            # Unicorn counts a whole IT block as one instruction, so run
            # sequences (which only branch at the end) to their end instead.
            uc.emu_start(CODE | thumb, CODE + code_len)
    except UcError as e:
        err = e.errno
    pc = uc.reg_read(UC_ARM_REG_PC)
    out_regs = [uc.reg_read(GPRS[i]) for i in range(15)] + [pc]
    out_ext = []
    for i in range(32):
        d = uc.reg_read(UC_ARM_REG_D0 + i)
        out_ext += [d & 0xFFFFFFFF, d >> 32]
    out_cpsr = uc.reg_read(UC_ARM_REG_CPSR)
    out_fpscr = uc.reg_read(UC_ARM_REG_FPSCR)
    in_thumb = out_cpsr & 0x20 != 0

    def insn_len():
        if not in_thumb:
            return 4
        hw = struct.unpack("<H", uc.mem_read(pc, 2))[0]
        return 4 if hw >> 11 >= 0b11101 else 2

    if interrupts:
        if interrupts[0] == 2:  # SVC; the PC is after it.
            if in_thumb:
                status = struct.unpack("<H", uc.mem_read(pc - 2, 2))[0] & 0xFF
            else:
                status = struct.unpack("<I", uc.mem_read(pc - 4, 4))[0] & 0xFFFFFF
        elif interrupts[0] == 7:  # BKPT
            status = -4
            out_regs[15] = pc + insn_len()
        else:
            return None  # Alignment fault.
    elif err is None:
        status = -1
    elif err in (ucc.UC_ERR_READ_UNMAPPED, ucc.UC_ERR_WRITE_UNMAPPED):
        status = -2
    elif err == ucc.UC_ERR_FETCH_UNMAPPED:
        # A branch to somewhere unmapped completed; the next fetch failed.
        status = -1 if pc != CODE else -2
    elif err == ucc.UC_ERR_INSN_INVALID:
        status = -3
        out_regs[15] = pc + insn_len()
    else:
        return None
    if bytes(uc.mem_read(CODE, len(code))) != code:
        return None  # Self-modifying code.
    if status == -1 and not in_thumb and pc & 3:
        return None  # Unpredictable interworking branch.
    mem = bytes(uc.mem_read(WIN, MEM_END - WIN))
    return status, out_regs, out_ext, out_cpsr, out_fpscr, mem


def mem_diff(mem):
    out = []
    if mem == BASE_MEM:
        return out
    i = 0
    n = len(mem)
    while i < n:
        if mem[i] != BASE_MEM[i]:
            j = i
            while j < n and mem[j] != BASE_MEM[j]:
                j += 1
            out.append(f"m{WIN + i:x}={mem[i:j].hex()}")
            i = j
        else:
            i += 1
    return out


def thumb_halfwords(code):
    """First halfwords of the Thumb instructions in `code`."""
    i = 0
    while i + 2 <= len(code):
        hw = struct.unpack("<H", code[i:i + 2])[0]
        yield hw
        i += 4 if hw >> 11 >= 0b11101 else 2


def is_it(hw):
    return hw >> 8 == 0xBF and hw & 0xF


def emit(out, thumb, code, code_len, seed, ov):
    if thumb:
        hws = list(thumb_halfwords(code[:code_len]))
        steps = len(hws)
        # A lone IT instruction can't be run on its own in unicorn, and
        # unicorn treats YIELD as an exception rather than a hint.
        if (steps == 1 and is_it(hws[0])) or 0xBF10 in hws:
            return False
    else:
        steps = code_len // 4
        if 0xE320F001 in struct.unpack(f"<{steps}I", code[:code_len]):
            return False
    regs, ext, tpidruro = initial_state(seed)
    for name, v in ov.items():
        regs[14 if name == "lr" else int(name[1:])] = v
    r = Rng(seed ^ 0xA5A5A5A5)
    cpsr = (r.next() & 0xF80F0000) | 0x10 | (0x20 if thumb else 0)
    control = r.next()
    fpscr = (r.next() & 0xF0000000) | (control & 0x03C00000 if control & 1 else 0)
    res = run_unicorn(thumb, code, code_len, steps, regs, ext, cpsr, fpscr, tpidruro)
    if res is None:
        return False
    status, oregs, oext, ocpsr, ofpscr, mem = res
    line = f"{'T' if thumb else 'A'} {seed:x} {code.hex()} {steps} {cpsr:x} {fpscr:x}"
    for name in sorted(ov):
        idx = 14 if name == "lr" else int(name[1:])
        line += f" r{idx}={regs[idx]:x}"
    line += f" : {status}"
    if status == -2:
        line += f" r15={oregs[15]:x}"
    else:
        line += "".join(f" r{i}={oregs[i]:x}" for i in range(16) if oregs[i] != regs[i])
        line += f" c={ocpsr & 0xFE0FFC20:x} f={ofpscr:x}"
        line += "".join(f" e{i}={oext[i]:x}" for i in range(64) if oext[i] != ext[i])
        line += "".join(" " + d for d in mem_diff(mem))
    out.append(line)
    return True


# Assembly templates. Each template returns (assembly, register overrides)
# or None. Register overrides are mostly used to point base registers into
# the data window.

rnd = random.Random(1234)
COND = ["eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le"]
INV = {c: COND[COND.index(c) ^ 1] for c in COND}
SHIFTS = ["lsl", "lsr", "asr", "ror"]
REGS = [f"r{i}" for i in range(13)] + ["lr"]
LOW = [f"r{i}" for i in range(8)]
DP3 = ["and", "eor", "sub", "rsb", "add", "adc", "sbc", "orr", "bic"]
TEST = ["tst", "teq", "cmp", "cmn"]


def R():
    return rnd.choice(REGS)


def L():
    return rnd.choice(LOW)


def S():
    return f"s{rnd.randrange(32)}"


def D():
    return f"d{rnd.randrange(32)}"


def pick(*xs):
    return rnd.choice(xs)


def s_flag():
    return pick("", "s")


def ror(v, n):
    n %= 32
    return ((v >> n) | (v << (32 - n))) & 0xFFFFFFFF


def arm_imm():
    return ror(rnd.randrange(256), 2 * rnd.randrange(16))


def thumb_imm():
    b = rnd.randrange(256)
    return pick(b, b | b << 16, (b << 8) | (b << 24), b * 0x01010101,
                ror(0x80 | rnd.randrange(128), rnd.randrange(8, 32)))


def shift_op():
    s = pick("lsl", "lsr", "asr", "ror", "rrx", "")
    if s in ("", "rrx"):
        return s and ", rrx"
    return f", {s} #{rnd.randrange(1, 33 if s in ('lsr', 'asr') else 32)}"


def two_regs():
    return rnd.sample(REGS, 2)


def window_reg(ov, align=1):
    """A register pointing into the data window."""
    r = rnd.choice([x for x in REGS if x not in ov])
    ov[r] = (0x4000 + rnd.randrange(0x18000)) & ~(align - 1)
    return r


def small_reg(ov):
    r = rnd.choice([x for x in REGS if x not in ov])
    ov[r] = rnd.randrange(-64, 64) & 0xFFFFFFFF
    return r


def reg_list(exclude, n):
    regs = rnd.sample([x for x in REGS if x not in exclude], n)
    return ", ".join(sorted(regs, key=REGS.index))


def mul_media_templates(c):
    """Templates valid in both instruction sets. `c` returns a condition
    suffix (always empty for Thumb)."""
    bt = lambda: pick("b", "t")
    rot = lambda: pick("", ", ror #8", ", ror #16", ", ror #24")
    lsb = lambda: rnd.randrange(32)
    return [
        lambda: (f"mul{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"{pick('mla', 'mls')}{c()} {R()}, {R()}, {R()}, {R()}", {}),
        lambda: (lambda a, b: (f"{pick('umull', 'umlal', 'smull', 'smlal', 'umaal')}{c()} {a}, {b}, {R()}, {R()}", {}))(*two_regs()),
        lambda: (f"smul{bt()}{bt()}{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"smla{bt()}{bt()}{c()} {R()}, {R()}, {R()}, {R()}", {}),
        lambda: (f"smulw{bt()}{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"smlaw{bt()}{c()} {R()}, {R()}, {R()}, {R()}", {}),
        lambda: (lambda a, b: (f"smlal{bt()}{bt()}{c()} {a}, {b}, {R()}, {R()}", {}))(*two_regs()),
        lambda: (f"{pick('smuad', 'smuadx', 'smusd', 'smusdx', 'smmul', 'smmulr')}{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"{pick('smlad', 'smladx', 'smlsd', 'smlsdx', 'smmla', 'smmlar', 'smmls', 'smmlsr')}{c()} {R()}, {R()}, {R()}, {R()}", {}),
        lambda: (lambda a, b: (f"{pick('smlald', 'smlaldx', 'smlsld', 'smlsldx')}{c()} {a}, {b}, {R()}, {R()}", {}))(*two_regs()),
        lambda: (f"{pick('sxtb', 'sxth', 'uxtb', 'uxth', 'sxtb16', 'uxtb16')}{c()} {R()}, {R()}{rot()}", {}),
        lambda: (f"{pick('sxtab', 'sxtah', 'uxtab', 'uxtah', 'sxtab16', 'uxtab16')}{c()} {R()}, {R()}, {R()}{rot()}", {}),
        lambda: (f"{pick('rev', 'rev16', 'revsh', 'rbit', 'clz')}{c()} {R()}, {R()}", {}),
        lambda: (lambda l: (f"bfc{c()} {R()}, #{l}, #{rnd.randrange(1, 33 - l)}", {}))(lsb()),
        lambda: (lambda l: (f"bfi{c()} {R()}, {R()}, #{l}, #{rnd.randrange(1, 33 - l)}", {}))(lsb()),
        lambda: (lambda l: (f"{pick('ubfx', 'sbfx')}{c()} {R()}, {R()}, #{l}, #{rnd.randrange(1, 33 - l)}", {}))(lsb()),
        lambda: (f"ssat{c()} {R()}, #{rnd.randrange(1, 33)}, {R()}{pick('', ', lsl #%d' % rnd.randrange(1, 32), ', asr #%d' % rnd.randrange(1, 32))}", {}),
        lambda: (f"usat{c()} {R()}, #{rnd.randrange(32)}, {R()}{pick('', ', lsl #%d' % rnd.randrange(1, 32), ', asr #%d' % rnd.randrange(1, 32))}", {}),
        lambda: (f"ssat16{c()} {R()}, #{rnd.randrange(1, 17)}, {R()}", {}),
        lambda: (f"usat16{c()} {R()}, #{rnd.randrange(16)}, {R()}", {}),
        lambda: (f"{pick('qadd', 'qsub', 'qdadd', 'qdsub')}{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"sel{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"pkhbt{c()} {R()}, {R()}, {R()}{pick('', ', lsl #%d' % rnd.randrange(1, 32))}", {}),
        lambda: (f"pkhtb{c()} {R()}, {R()}, {R()}, asr #{rnd.randrange(1, 33)}", {}),
        lambda: (f"usad8{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"usada8{c()} {R()}, {R()}, {R()}, {R()}", {}),
        lambda: (f"{pick('s', 'q', 'sh', 'u', 'uq', 'uh')}{pick('add16', 'asx', 'sax', 'sub16', 'add8', 'sub8')}{c()} {R()}, {R()}, {R()}", {}),
        lambda: (f"msr{c()} {pick('apsr_nzcvq', 'apsr_g', 'apsr_nzcvqg')}, {R()}", {}),
        lambda: (f"mrs{c()} {R()}, apsr", {}),
    ]


def load_store_templates(arm, c):
    def single():
        ov = {}
        s = pick("", "b", "h", "sb", "sh")
        op = ("ldr" if s in ("sb", "sh") or rnd.random() < 0.5 else "str") + s
        b = window_reg(ov)
        rt = rnd.choice([x for x in REGS if x != b])
        lim = 4096 if s in ("", "b") and arm else 256
        imm = rnd.randrange(lim) * pick(1, -1)
        form = rnd.randrange(5 if arm else 4)
        if form == 0:
            return f"{op}{c()} {rt}, [{b}, #{imm}]", ov
        if form == 1:
            return f"{op}{c()} {rt}, [{b}, #{imm}]!", ov
        if form == 2:
            return f"{op}{c()} {rt}, [{b}], #{imm}", ov
        m = small_reg(ov)
        if not arm:
            return f"{op} {rt}, [{b}, {m}{pick('', ', lsl #%d' % rnd.randrange(1, 4))}]", ov
        sh = f", {pick(*SHIFTS)} #{rnd.randrange(1, 32)}" if s in ("", "b") else ""
        sign = pick("", "-")
        if form == 3:
            return f"{op}{c()} {rt}, [{b}, {sign}{m}{sh}]{pick('', '!')}", ov
        return f"{op}{c()} {rt}, [{b}], {sign}{m}{sh}", ov

    def dual():
        ov = {}
        b = window_reg(ov, 4)
        if arm:
            i = pick(0, 2, 4, 6, 8, 10)
            r1, r2 = f"r{i}", f"r{i + 1}"
        else:
            r1, r2 = rnd.sample([x for x in REGS if x != "lr"], 2)
        if b in (r1, r2):
            return None
        op = pick("ldrd", "strd")
        imm = rnd.randrange(64) * 4 * pick(1, -1)
        return pick(f"{op}{c()} {r1}, {r2}, [{b}, #{imm}]",
                    f"{op}{c()} {r1}, {r2}, [{b}, #{imm}]!",
                    f"{op}{c()} {r1}, {r2}, [{b}], #{imm}"), ov

    def multiple():
        ov = {}
        b = window_reg(ov, 4)
        mode = pick("ia", "ib", "da", "db") if arm else pick("ia", "db")
        n = rnd.randrange(1 if arm else 2, 6)
        return f"{pick('ldm', 'stm')}{mode}{c()} {b}{pick('', '!')}, {{{reg_list([b], n)}}}", ov

    def push_pop():
        return f"{pick('push', 'pop')}{c()} {{{reg_list([], rnd.randrange(1, 6))}}}", {}

    def literal():
        if arm:
            return f"{pick('ldr', 'ldrb')}{c()} {R()}, [pc, #{rnd.randrange(-255, 256)}]", {}
        return f"{pick('ldr', 'ldrb', 'ldrh', 'ldrsb', 'ldrsh')} {R()}, [pc, #{rnd.randrange(256) * 4}]", {}

    def exclusive():
        ov = {}
        b = window_reg(ov, 8)
        sz = pick("", "b", "h", "d")
        if sz == "d":
            i = pick(0, 2, 4, 6, 8, 10)
            used = [b, f"r{i}", f"r{i + 1}"]
            rt = f"r{i}, r{i + 1}"
        else:
            rt = rnd.choice([x for x in REGS if x != b])
            used = [b, rt]
        if len(set(used)) != len(used):
            return None
        status = rnd.choice([x for x in REGS if x not in used])
        seq = [f"ldrex{sz} {rt}, [{b}]", f"strex{sz} {status}, {rt}, [{b}]"]
        k = rnd.random()
        if k < 0.25:
            seq.insert(1, "clrex")
        elif k < 0.5:
            seq = seq[1:]
        return "\n".join(seq), ov

    return [single] * 6 + [dual, multiple, multiple, push_pop, literal, exclusive]


def branch_templates(arm, c):
    def target(span):
        return CODE + rnd.randrange(-span, span) * 4

    def reg_branch(op):
        ov = {}
        r = rnd.choice(REGS)
        ov[r] = (0x4000 + rnd.randrange(0x18000)) & ~3 | rnd.randrange(2)
        return f"{op}{c()} {r}", ov

    def load_pc():
        ov = {}
        b = window_reg(ov, 4)
        return f"ldr{c()} pc, [{b}, #{rnd.randrange(64) * 4}]", ov

    def mov_pc():
        ov = {}
        r = window_reg(ov, 4 if arm else 2)
        return f"mov{c()} pc, {r}", ov

    t = [
        lambda: (f"b{c()} {target(0x800):#x}", {}),
        lambda: (f"bl{c()} {target(0x800):#x}", {}),
        lambda: (f"blx {target(0x800):#x}", {}),
        lambda: reg_branch("bx"),
        lambda: reg_branch("blx"),
        load_pc,
        lambda: (f"pop{c()} {{{rnd.choice(REGS[:-1])}, pc}}", {}),
        mov_pc,
    ]
    if arm:
        t.append(lambda: (lambda ov: (f"add{c()} pc, {window_reg(ov, 4)}, #{rnd.randrange(64) * 4}", ov))({}))
    else:
        def table():
            ov = {}
            m = rnd.choice([x for x in REGS if x != "lr"])
            ov[m] = rnd.randrange(8)
            # The table follows the instruction.
            ov["_tail"] = bytes(rnd.randrange(32) for _ in range(16))
            return pick(f"tbb [pc, {m}]", f"tbh [pc, {m}, lsl #1]"), ov

        t += [
            lambda: (f"{pick('cbz', 'cbnz')} {L()}, {CODE + 4 + rnd.randrange(64) * 2:#x}", {}),
            lambda: (f"b{pick(*COND)} {CODE + 4 + rnd.randrange(-128, 127) * 2:#x}", {}),
            lambda: (f"b{pick(*COND)}.w {CODE + 4 + rnd.randrange(-0x8000, 0x8000) * 2:#x}", {}),
            lambda: (f"b.w {CODE + 4 + rnd.randrange(-0x8000, 0x8000) * 2:#x}", {}),
            table,
        ]
    return t


def cp15dmb(arm, rt):
    """`mcr p15, 0, rt, c7, c10, 5` machine code."""
    if arm:
        return struct.pack("<I", 0xEE070FBA | rt << 12)
    return struct.pack("<HH", 0xEE07, 0x0FBA | rt << 12)


def misc_templates(arm, c):
    return [
        lambda: (f"svc{c()} #{rnd.randrange(1 << 24) if arm else rnd.randrange(256)}", {}),
        lambda: (f"bkpt #{rnd.randrange(256)}", {}),
        lambda: (f"udf #{rnd.randrange(256)}", {}),
        lambda: (f"mrc{c()} p15, 0, {R()}, c13, c0, 3", {}),
        # CP15DMB. Keystone can crash on this deprecated form, so encode it
        # directly.
        lambda: (cp15dmb(arm, rnd.randrange(13)), {}),
        lambda: (pick("dmb", "dsb", "isb", "nop", "clrex", f"pld [{R()}]", "sev"), {}),
    ]


def vfp_templates(arm, c):
    t = []
    for ty, reg in (("f32", S), ("f64", D)):
        t += [
            lambda ty=ty, reg=reg: (f"{pick('vadd', 'vsub', 'vmul', 'vnmul', 'vdiv', 'vmla', 'vmls', 'vnmla', 'vnmls')}{c()}.{ty} {reg()}, {reg()}, {reg()}", {}),
        ] * 8
        t += [
            lambda ty=ty, reg=reg: (f"{pick('vmov', 'vabs', 'vneg', 'vsqrt', 'vcmp', 'vcmpe')}{c()}.{ty} {reg()}, {reg()}", {}),
            lambda ty=ty, reg=reg: (f"{pick('vcmp', 'vcmpe')}{c()}.{ty} {reg()}, #0", {}),
            lambda ty=ty, reg=reg: (f"vmov{c()}.{ty} {reg()}, #{pick(0.5, 1.0, -2.0, 0.125, 31.0, -1.5, 3.0, 0.25, 17.0, -0.1875)}", {}),
        ]

    def ld_st():
        ov = {}
        b = window_reg(ov, 4)
        return f"{pick('vldr', 'vstr')}{c()} {pick(S, D)()}, [{b}, #{rnd.randrange(-255, 256) * 4}]", ov

    def ext_list():
        dbl = rnd.random() < 0.5
        n = rnd.randrange(1, 9)
        first = rnd.randrange(32 - n + 1)
        return ", ".join(f"{'d' if dbl else 's'}{first + k}" for k in range(n))

    def multiple():
        ov = {}
        b = window_reg(ov, 4)
        mode, wb = pick(("ia", ""), ("ia", "!"), ("db", "!"))
        return f"{pick('vldm', 'vstm')}{mode}{c()} {b}{wb}, {{{ext_list()}}}", ov

    def neon():
        reg = (lambda: f"q{rnd.randrange(16)}") if rnd.random() < 0.3 else D
        return f"{pick('vadd', 'vsub', 'vmul')}.f32 {reg()}, {reg()}, {reg()}", {}

    def compare_branch():
        # A float comparison feeding a conditional instruction.
        ty, reg = pick(("f32", S), ("f64", D))
        cond = pick(*COND)
        it = "" if arm else f"it {cond}\n"
        return (f"{pick('vcmp', 'vcmpe')}.{ty} {reg()}, {pick(reg(), '#0')}\n"
                f"vmrs APSR_nzcv, fpscr\n{it}mov{cond} {R()}, #1"), {}

    t += [
        lambda: (f"vcvt{c()}.f64.f32 {D()}, {S()}", {}),
        lambda: (f"vcvt{c()}.f32.f64 {S()}, {D()}", {}),
        lambda: (lambda ty: (f"vcvt{c()}.{ty}.{pick('s32', 'u32')} {S() if ty == 'f32' else D()}, {S()}", {}))(pick("f32", "f64")),
        lambda: (lambda ty: (f"{pick('vcvt', 'vcvtr')}{c()}.{pick('s32', 'u32')}.{ty} {S()}, {S() if ty == 'f32' else D()}", {}))(pick("f32", "f64")),
    ] * 3
    t += [
        compare_branch,
        compare_branch,
        lambda: (f"vmov{c()} {R()}, {S()}", {}),
        lambda: (f"vmov{c()} {S()}, {R()}", {}),
        lambda: (lambda a, b: (f"vmov{c()} {a}, {b}, {D()}", {}))(*two_regs()),
        lambda: (f"vmov{c()} {D()}, {R()}, {R()}", {}),
        lambda: (lambda a, b, s: (f"vmov{c()} {a}, {b}, s{s}, s{s + 1}", {}))(*two_regs(), rnd.randrange(31)),
        lambda: (lambda s: (f"vmov{c()} s{s}, s{s + 1}, {R()}, {R()}", {}))(rnd.randrange(31)),
        lambda: (f"vmov{c()}.32 {D()}[{rnd.randrange(2)}], {R()}", {}),
        lambda: (f"vmov{c()}.32 {R()}, {D()}[{rnd.randrange(2)}]", {}),
        lambda: (f"vmrs{c()} {pick(R(), 'apsr_nzcv')}, fpscr", {}),
        # Only the bits the interpreter models (no vector length/stride).
        lambda: (lambda r: (f"vmsr{c()} fpscr, {r}", {r: rnd.randrange(1 << 32) & 0xF3C0009F}))(R()),
        lambda: (f"vldr{c()} {pick(S, D)()}, [pc, #{rnd.randrange(64) * 4}]", {}),
        lambda: (f"{pick('vpush', 'vpop')}{c()} {{{ext_list()}}}", {}),
        ld_st, ld_st, multiple, neon, neon,
    ]
    return t


def arm_templates():
    c = lambda: pick(*COND) if rnd.random() < 0.4 else ""
    t = [
        lambda: (f"{pick(*DP3, 'rsc')}{s_flag()}{c()} {R()}, {R()}, #{arm_imm()}", {}),
        lambda: (f"{pick('mov', 'mvn')}{s_flag()}{c()} {R()}, #{arm_imm()}", {}),
        lambda: (f"{pick(*TEST)}{c()} {R()}, #{arm_imm()}", {}),
        lambda: (f"{pick(*DP3, 'rsc')}{s_flag()}{c()} {R()}, {R()}, {R()}{shift_op()}", {}),
        lambda: (f"{pick('mov', 'mvn')}{s_flag()}{c()} {R()}, {R()}{shift_op()}", {}),
        lambda: (f"{pick(*TEST)}{c()} {R()}, {R()}{shift_op()}", {}),
        lambda: (f"{pick(*DP3, 'rsc')}{s_flag()}{c()} {R()}, {R()}, {R()}, {pick(*SHIFTS)} {R()}", {}),
        lambda: (f"{pick('mov', 'mvn')}{s_flag()}{c()} {R()}, {R()}, {pick(*SHIFTS)} {R()}", {}),
        lambda: (f"{pick(*TEST)}{c()} {R()}, {R()}, {pick(*SHIFTS)} {R()}", {}),
        lambda: (f"{pick('add', 'sub')}{c()} {R()}, pc, #{rnd.randrange(256)}", {}),
        lambda: (f"mov{c()} {R()}, pc", {}),
        lambda: (f"movw{c()} {R()}, #{rnd.randrange(65536)}", {}),
        lambda: (f"movt{c()} {R()}, #{rnd.randrange(65536)}", {}),
        lambda: (f"msr{c()} {pick('apsr_nzcvq', 'apsr_g', 'apsr_nzcvqg')}, #{arm_imm()}", {}),
        lambda: (pick(f"muls{c()} {R()}, {R()}, {R()}", f"mlas{c()} {R()}, {R()}, {R()}, {R()}"), {}),
        lambda: (lambda a, b: (f"{pick('umulls', 'umlals', 'smulls', 'smlals')}{c()} {a}, {b}, {R()}, {R()}", {}))(*two_regs()),
    ]
    t += mul_media_templates(c)
    t += load_store_templates(True, c)
    t += branch_templates(True, c)
    t += misc_templates(True, c)
    t += vfp_templates(True, c)
    return t


def thumb_templates():
    c = lambda: ""
    t = [
        # 16-bit forms (flag-setting, outside IT blocks).
        lambda: (f"{pick('adds', 'subs')} {L()}, {L()}, {pick(L(), '#%d' % rnd.randrange(8))}", {}),
        lambda: (f"{pick('movs', 'cmp', 'adds', 'subs')} {L()}, #{rnd.randrange(256)}", {}),
        lambda: (f"{pick('lsls', 'lsrs', 'asrs')} {L()}, {L()}, #{rnd.randrange(1, 32)}", {}),
        lambda: (f"{pick('ands', 'eors', 'lsls', 'lsrs', 'asrs', 'adcs', 'sbcs', 'rors', 'tst', 'cmp', 'cmn', 'orrs', 'bics', 'mvns')} {L()}, {L()}", {}),
        lambda: (f"rsbs {L()}, {L()}, #0", {}),
        lambda: (lambda a: (f"muls {a}, {L()}, {a}", {}))(L()),
        lambda: (f"{pick('add', 'mov', 'cmp')} {R()}, {R()}", {}),
        lambda: (f"{pick('add', 'sub')} sp, #{rnd.randrange(128) * 4}", {}),
        lambda: (f"add {L()}, sp, #{rnd.randrange(256) * 4}", {}),
        lambda: (f"adr {L()}, {CODE + 4 + rnd.randrange(256) * 4:#x}", {}),
        lambda: (f"{pick('sxtb', 'sxth', 'uxtb', 'uxth', 'rev', 'rev16', 'revsh')} {L()}, {L()}", {}),
        lambda: (f"{pick('ldr', 'str')} {L()}, [sp, #{rnd.randrange(256) * 4}]", {}),
        # 32-bit forms.
        lambda: (f"{pick(*DP3, 'orn')}{s_flag()}.w {R()}, {R()}, #{thumb_imm()}", {}),
        lambda: (f"{pick('mov', 'mvn')}{s_flag()}.w {R()}, #{thumb_imm()}", {}),
        lambda: (f"{pick(*TEST)}.w {R()}, #{thumb_imm()}", {}),
        lambda: (f"{pick(*DP3, 'orn')}{s_flag()}.w {R()}, {R()}, {R()}{shift_op()}", {}),
        lambda: (f"{pick('mov', 'mvn')}{s_flag()}.w {R()}, {R()}{shift_op()}", {}),
        lambda: (f"{pick(*TEST)}.w {R()}, {R()}{shift_op()}", {}),
        lambda: (f"{pick('lsl', 'lsr', 'asr', 'ror')}{s_flag()}.w {R()}, {R()}, {R()}", {}),
        lambda: (f"{pick('addw', 'subw')} {R()}, {R()}, #{rnd.randrange(4096)}", {}),
        lambda: (f"{pick('addw', 'subw')} {R()}, pc, #{rnd.randrange(4096)}", {}),
        lambda: (f"movw {R()}, #{rnd.randrange(65536)}", {}),
        lambda: (f"movt {R()}, #{rnd.randrange(65536)}", {}),
    ]
    t += mul_media_templates(c)
    t += load_store_templates(False, c)
    t += branch_templates(False, c)
    t += misc_templates(False, c)
    t += vfp_templates(False, c)

    def it_block():
        cond = pick(*COND)
        pattern = "".join(pick("t", "e") for _ in range(rnd.randrange(4)))
        conds = [cond] + [cond if p == "t" else INV[cond] for p in pattern]
        body = [pick(
            f"add{cd} {L()}, {L()}",
            f"mov{cd} {L()}, #{rnd.randrange(256)}",
            f"add{cd} {L()}, {L()}, #{rnd.randrange(8)}",
            f"cmp{cd} {L()}, {L()}",
            f"add{cd}.w {R()}, {R()}, #{thumb_imm()}",
            f"adds{cd}.w {R()}, {R()}, {R()}",
            f"lsl{cd} {L()}, {L()}, #{rnd.randrange(1, 31)}",
            f"vadd{cd}.f32 {S()}, {S()}, {S()}",
            f"mrc{cd} p15, 0, {R()}, c13, c0, 3",
        ) for cd in conds]
        pre = pick("", f"cmp {L()}, {L()}\n")
        return pre + f"it{pattern} {cond}\n" + "\n".join(body), {}

    t += [it_block] * 6
    return t


ks_arm = Ks(KS_ARCH_ARM, KS_MODE_ARM)
ks_thumb = Ks(KS_ARCH_ARM, KS_MODE_THUMB)


def assemble(thumb, asm):
    if isinstance(asm, bytes):
        return asm
    try:
        enc, _ = (ks_thumb if thumb else ks_arm).asm(asm, CODE)
    except KsError:
        return None
    return bytes(enc) if enc else None


def main():
    out = []
    seeds = random.Random(99)
    counts = {}
    skipped = 0

    for thumb, templates, per in ((False, arm_templates(), 140), (True, thumb_templates(), 120)):
        n0 = len(out)
        for f in templates:
            done = tries = 0
            while done < per and tries < per * 4:
                tries += 1
                r = f()
                if r is None:
                    continue
                asm, ov = r
                code = assemble(thumb, asm)
                if code is None:
                    continue
                n = len(code)
                code += ov.pop("_tail", b"")
                if emit(out, thumb, code, n, seeds.randrange(1, 1 << 32), ov):
                    done += 1
                else:
                    skipped += 1
        counts["thumb" if thumb else "arm"] = len(out) - n0

    with open(sys.argv[1], "w") as f:
        f.write("\n".join(out) + "\n")
    print(counts, "skipped", skipped, file=sys.stderr)


if __name__ == "__main__":
    main()
