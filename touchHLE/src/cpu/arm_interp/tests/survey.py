# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Surveys the instructions used by guest binaries.

Usage:

    uv run --with lief --with capstone python -I survey.py \\
        guest_samples.json <Mach-O files...>

For each file it disassembles the armv7 slice's `__text` (falling back to
armv6), using the symbol table to find function boundaries and whether each
function is Thumb (N_ARM_THUMB_DEF), and skipping literal pools, jump tables
and LC_DATA_IN_CODE ranges. It prints a table of (instruction set,
mnemonic, count, example), and writes up to 40 distinct encodings per row
to the JSON file, to compare with what `gen_vectors.py` covers. The JSON
holds code from the guest binaries, so it is not committed. Rows for VFP and Advanced SIMD
instructions include the operand shape and are marked VFP or NEON.
"""

import collections
import json
import random
import re
import sys

import lief
from capstone import CS_ARCH_ARM, CS_MODE_ARM, CS_MODE_THUMB, Cs
from capstone.arm import (ARM_INS_LDR, ARM_INS_LDRB, ARM_INS_LDRD, ARM_INS_LDRH,
                          ARM_INS_LDRSB, ARM_INS_LDRSH, ARM_INS_TBB, ARM_INS_TBH,
                          ARM_INS_VLDR, ARM_OP_MEM, ARM_REG_PC)

LITERAL_LOADS = (ARM_INS_LDR, ARM_INS_LDRD, ARM_INS_VLDR, ARM_INS_LDRH,
                 ARM_INS_LDRB, ARM_INS_LDRSH, ARM_INS_LDRSB)


def pick_slice(fat):
    fallback = None
    for b in fat:
        if b.header.cpu_subtype == 9:  # armv7
            return b
        if b.header.cpu_subtype == 6:  # armv6
            fallback = b
    return fallback


def is_neon(ins, thumb):
    if ins.size != 4:
        return False
    if thumb:
        h1 = int.from_bytes(ins.bytes[0:2], "little")
        return (h1 >> 8) in (0xEF, 0xFF) or ((h1 >> 8) == 0xF9 and not h1 & 0x10)
    w = int.from_bytes(ins.bytes, "little")
    return (w >> 25) == 0x79 or (w >> 24) == 0xF4


def row_name(ins, thumb):
    """Mnemonic, plus operand shape for VFP/NEON instructions."""
    name = ins.insn_name()
    neon = is_neon(ins, thumb)
    if name[0] not in "vf" and not neon:
        return name
    shape = re.sub(r"\b(r\d+|sb|sl|fp|ip|sp|lr|pc)\b", "R", ins.op_str)
    shape = re.sub(r"\bs\d+\b", "S", shape)
    shape = re.sub(r"\bd\d+\b", "D", shape)
    shape = re.sub(r"\bq\d+\b", "Q", shape)
    shape = re.sub(r"#-?[0-9a-fx.e+-]+", "#", shape)
    shape = re.sub(r"\{[SDQ, -]+\}", "{list}", shape)
    mnemonic = re.sub(r"(eq|ne|hs|lo|mi|pl|vs|vc|hi|ls|ge|lt|gt|le)(?=\.|$)", "", ins.mnemonic)
    return f"{'NEON' if neon else 'VFP'} {mnemonic} {shape}"


def survey(path, counts, examples, samples, rnd):
    fat = lief.MachO.parse(path)  # Must outlive `b`.
    b = pick_slice(fat)
    text = b.get_section("__text")
    base, size = text.virtual_address, text.size
    data = bytes(text.content)
    sect_idx = next(i + 1 for i, s in enumerate(b.sections)
                    if s.name == "__text" and s.virtual_address == base)
    funcs = {}
    for s in b.symbols:
        if s.raw_type & 0xE0 or (s.raw_type & 0x0E) != 0x0E:
            continue  # Debugging symbol, or not defined in a section.
        if s.numberof_sections == sect_idx and base <= s.value < base + size:
            funcs[s.value] = funcs.get(s.value, False) or bool(s.description & 0x8)
    funcs.setdefault(base, False)
    starts = sorted(funcs)

    data_bytes = set()
    if b.has_data_in_code:
        for e in b.data_in_code.entries:
            a = e.offset - text.offset + base
            data_bytes.update(range(a, a + e.length))

    arm = Cs(CS_ARCH_ARM, CS_MODE_ARM)
    thumb = Cs(CS_ARCH_ARM, CS_MODE_THUMB)
    arm.detail = thumb.detail = True

    def sweep(collect):
        for i, start in enumerate(starts):
            end = starts[i + 1] if i + 1 < len(starts) else base + size
            th = funcs[start]
            md = thumb if th else arm
            step = 2 if th else 4
            a = start
            while a < end:
                ins = None
                if a not in data_bytes:
                    off = a - base
                    ins = next(md.disasm(data[off:min(end - base, off + 4)], a, 1), None)
                if ins is None or any(x in data_bytes for x in range(a, a + ins.size)):
                    a += step
                    continue
                if collect:
                    mode = ("T16" if ins.size == 2 else "T32") if th else "A32"
                    key = (mode, row_name(ins, th))
                    counts[key] += 1
                    hx = ins.bytes.hex()
                    examples.setdefault(key, f"{ins.mnemonic} {ins.op_str} [{hx}]")
                    lst = samples.setdefault(key, [])
                    if hx not in lst:
                        if len(lst) < 40:
                            lst.append(hx)
                        elif (j := rnd.randrange(counts[key])) < 40:
                            lst[j] = hx
                else:
                    find_data(ins, a, th, end)
                a += ins.size

    def find_data(ins, a, th, end):
        """Records literal pools and jump tables referenced by `ins`."""
        if ins.id in LITERAL_LOADS:
            for op in ins.operands:
                if op.type == ARM_OP_MEM and op.mem.base == ARM_REG_PC and op.mem.index == 0:
                    t = ((a + (4 if th else 8)) & ~3) + op.mem.disp
                    n = 8 if ins.id in (ARM_INS_LDRD, ARM_INS_VLDR) and ins.op_str.startswith("d") else 4
                    data_bytes.update(range(t, t + n))
        if ins.id in (ARM_INS_TBB, ARM_INS_TBH):
            table = a + 4
            entry = 1 if ins.id == ARM_INS_TBB else 2
            limit, p = end, table
            while p < limit:
                e = int.from_bytes(data[p - base:p - base + entry], "little")
                limit = min(limit, table + 2 * e)
                p += entry
            data_bytes.update(range(table, (p + 1) & ~1))
        if not th and ins.id == ARM_INS_LDR and ins.operands[0].reg == ARM_REG_PC and "[pc, r" in ins.op_str:
            p = a + 8
            while p < end and base <= int.from_bytes(data[p - base:p - base + 4], "little") < base + size:
                data_bytes.update(range(p, p + 4))
                p += 4

    sweep(False)
    sweep(True)


def main():
    counts = collections.Counter()
    examples = {}
    samples = {}
    rnd = random.Random(1)
    for path in sys.argv[2:]:
        before = sum(counts.values())
        survey(path, counts, examples, samples, rnd)
        print(f"{path}: {sum(counts.values()) - before} instructions", file=sys.stderr)
    with open(sys.argv[1], "w") as f:
        json.dump({f"{m}|{n}": v for (m, n), v in samples.items()}, f)
    for (mode, name), n in sorted(counts.items(), key=lambda kv: (kv[0][0], -kv[1])):
        print(f"{mode}\t{name}\t{n}\t{examples[(mode, name)]}")


if __name__ == "__main__":
    main()
