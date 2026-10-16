/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Replays the differential test vectors generated with unicorn by
//! `gen_vectors.py`. See that script for the file format and the known
//! differences from unicorn.

use touchHLE_arm_interp::{Context, Interp};

const CODE: usize = 0x1000;
const WIN: usize = 0x2000;
const MEM_END: usize = 0x20000;

fn xorshift(mut s: u32) -> u32 {
    s ^= s << 13;
    s ^= s >> 17;
    s ^= s << 5;
    s
}

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = xorshift(self.0);
        self.0
    }
}

fn base_memory() -> Vec<u8> {
    let mut s = 0x12345678;
    let mut out = Vec::with_capacity(MEM_END - WIN);
    for _ in 0..(MEM_END - WIN) / 4 {
        s = xorshift(s);
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

const SPECIAL: [u32; 7] = [0, 1, 0x7fffffff, 0x80000000, 0xffffffff, 0x8000, 0xffff];
const F32S: [u32; 18] = [
    0x00000000, 0x80000000, 0x3F800000, 0xBF800000, 0x7F800000, 0xFF800000, 0x7FC00000, 0x7F800001,
    0xFFC00001, 0x00000001, 0x807FFFFF, 0x00800000, 0x7F7FFFFF, 0x4B000000, 0x4F000000, 0xCF000000,
    0x3F000000, 0x4EFFFFFF,
];
const F64S: [u64; 17] = [
    0x0000000000000000,
    0x8000000000000000,
    0x3FF0000000000000,
    0x7FF0000000000000,
    0xFFF0000000000000,
    0x7FF8000000000000,
    0x7FF0000000000001,
    0x0000000000000001,
    0x0010000000000000,
    0x7FEFFFFFFFFFFFFF,
    0x41E0000000000000,
    0xC1E0000000000000,
    0x41EFFFFFFFE00000,
    0x3FE0000000000000,
    0x36A0000000000000,
    0x47EFFFFFE0000000,
    0x3810000000000000,
];

/// Mirrors `initial_state` in gen_vectors.py.
fn initial_state(seed: u32) -> ([u32; 16], [u32; 64], u32) {
    let mut r = Rng(if seed == 0 { 1 } else { seed });
    let mut regs = [0; 16];
    for reg in regs.iter_mut() {
        let c = r.next() % 8;
        *reg = if c < 3 {
            0x4000 + r.next() % 0x18000
        } else if c < 5 {
            ((r.next() % 512) as i32 - 256) as u32
        } else if c == 5 {
            SPECIAL[(r.next() % 7) as usize]
        } else {
            r.next()
        };
    }
    regs[13] = 0x10000 + (r.next() % 0x1000) * 4;
    regs[15] = CODE as u32;
    let mut ext = [0; 64];
    for d in 0..32 {
        let pair = match r.next() % 6 {
            0 => [r.next(), r.next()],
            1 => [
                F32S[(r.next() % 18) as usize],
                F32S[(r.next() % 18) as usize],
            ],
            2 => {
                let v = F64S[(r.next() % 17) as usize];
                [v as u32, (v >> 32) as u32]
            }
            3 => {
                let a = ((r.next() % 2001) as i32 - 1000) as f32 / 16.0;
                let b = ((r.next() % 2001) as i32 - 1000) as f32 / 16.0;
                [a.to_bits(), b.to_bits()]
            }
            4 => {
                let v = (((r.next() % 20001) as i32 - 10000) as f64 / 64.0).to_bits();
                [v as u32, (v >> 32) as u32]
            }
            _ => {
                let mut f = || {
                    let sign = r.next() & 0x80000000;
                    let exp = 117 + r.next() % 20;
                    sign | exp << 23 | (r.next() & 0x7fffff)
                };
                [f(), f()]
            }
        };
        ext[2 * d] = pair[0];
        ext[2 * d + 1] = pair[1];
    }
    let tpidruro = r.next();
    (regs, ext, tpidruro)
}

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap()
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Step through the instructions one at a time.
    Step,
    /// Like [Mode::Step], but with an SVC after the code.
    StepTrailer,
    /// Run sequences that complete normally with an SVC after them and a
    /// large tick budget, which exercises decoding and running whole blocks.
    Block,
}

#[allow(clippy::needless_range_loop)]
/// Runs one vector line, returning a description of any mismatch.
///
/// Returns `Ok(false)` if the vector isn't suitable for the mode.
fn run_case(line: &str, base: &[u8], mem: &mut [u8], mode: Mode) -> Result<bool, String> {
    let (input, expected) = line.split_once(" : ").unwrap();
    let mut it = input.split(' ');
    let thumb = it.next().unwrap() == "T";
    let seed = hex(it.next().unwrap());
    let code = hex_bytes(it.next().unwrap());
    let steps: u32 = it.next().unwrap().parse().unwrap();
    let cpsr = hex(it.next().unwrap());
    let fpscr = hex(it.next().unwrap());
    let (mut regs, mut ext, tpidruro) = initial_state(seed);
    for ov in it {
        let (r, v) = ov[1..].split_once('=').unwrap();
        regs[r.parse::<usize>().unwrap()] = hex(v);
    }
    assert_eq!(thumb, cpsr & 0x20 != 0);
    let initial_regs = regs;
    let initial_ext = ext;

    let mut it = expected.split(' ');
    let mut want_status: i32 = it.next().unwrap().parse().unwrap();
    let mut want_cpsr = None;
    let mut want_fpscr = None;
    let mut want_mem = base.to_vec();
    for item in it {
        let (k, v) = item.split_once('=').unwrap();
        match k.as_bytes()[0] {
            b'r' => regs[k[1..].parse::<usize>().unwrap()] = hex(v),
            b'e' => ext[k[1..].parse::<usize>().unwrap()] = hex(v),
            b'c' => want_cpsr = Some(hex(v)),
            b'f' => want_fpscr = Some(hex(v)),
            b'm' => {
                let a = usize::from_str_radix(&k[1..], 16).unwrap() - WIN;
                let bytes = hex_bytes(v);
                want_mem[a..a + bytes.len()].copy_from_slice(&bytes);
            }
            _ => panic!("bad item {item}"),
        }
    }

    mem[..WIN].fill(0);
    mem[CODE..CODE + code.len()].copy_from_slice(&code);
    mem[WIN..].copy_from_slice(base);
    if mode != Mode::Step {
        let end = CODE + code.len();
        if want_status != -1 || regs[15] != end as u32 || want_cpsr.unwrap() & 0x20 != cpsr & 0x20 {
            return Ok(false);
        }
        let svc: &[u8] = if thumb {
            &[0x00, 0xdf]
        } else {
            &[0, 0, 0, 0xef]
        };
        mem[end..end + svc.len()].copy_from_slice(svc);
        if mode == Mode::Block {
            want_status = 0;
            regs[15] += svc.len() as u32;
        }
    }

    let mut interp = unsafe { Interp::new(mem.as_mut_ptr(), MEM_END as u64, 1) };
    let mut ctx = Context {
        regs: initial_regs,
        extregs: initial_ext,
        cpsr,
        fpscr,
    };
    interp.swap_context(&mut ctx);
    interp.set_tpidruro(tpidruro);
    let mut status = -1;
    if mode == Mode::Block {
        let mut ticks = 1000;
        status = interp.run_or_step(Some(&mut ticks));
    } else {
        for _ in 0..steps {
            status = interp.run_or_step(None);
            if status != -1 {
                break;
            }
        }
    }
    let mut ctx = Context::new();
    interp.swap_context(&mut ctx);
    drop(interp);

    let mut errors = Vec::new();
    if status != want_status {
        errors.push(format!("status {status}, expected {want_status}"));
    }
    if want_status == -2 {
        if ctx.regs[15] != regs[15] {
            errors.push(format!("pc {:#x}, expected {:#x}", ctx.regs[15], regs[15]));
        }
    } else {
        for i in 0..16 {
            if ctx.regs[i] != regs[i] {
                errors.push(format!("r{i} {:#x}, expected {:#x}", ctx.regs[i], regs[i]));
            }
        }
        let c = ctx.cpsr & 0xfe0f_fc20;
        if Some(c) != want_cpsr {
            errors.push(format!("cpsr {c:#x}, expected {:#x}", want_cpsr.unwrap()));
        }
        if Some(ctx.fpscr) != want_fpscr {
            errors.push(format!(
                "fpscr {:#x}, expected {:#x}",
                ctx.fpscr,
                want_fpscr.unwrap()
            ));
        }
        for i in 0..64 {
            if ctx.extregs[i] != ext[i] {
                errors.push(format!(
                    "e{i} {:#x}, expected {:#x}",
                    ctx.extregs[i], ext[i]
                ));
            }
        }
        if mem[WIN..] != want_mem[..] {
            let first = (0..want_mem.len())
                .find(|&i| mem[WIN + i] != want_mem[i])
                .unwrap();
            errors.push(format!("memory differs from {:#x}", WIN + first));
        }
    }
    if errors.is_empty() {
        Ok(true)
    } else {
        Err(errors.join(", "))
    }
}

fn run_all(block: bool) {
    let mode = if block { Mode::Block } else { Mode::Step };
    let text = include_str!("vectors.txt");
    let base = base_memory();
    let mut mem = vec![0u8; MEM_END];
    let mut failures = Vec::new();
    let mut total = 0;
    for line in text.lines() {
        // Some instructions read the bytes after them (TBB tables, literal
        // loads), which the SVC changes; skip those in block mode.
        if block && run_case(line, &base, &mut mem, Mode::StepTrailer) != Ok(true) {
            continue;
        }
        match run_case(line, &base, &mut mem, mode) {
            Ok(ran) => total += ran as usize,
            Err(e) => {
                total += 1;
                failures.push(format!(
                    "{line}
    {e}"
                ));
            }
        }
    }
    let shown = if std::env::var_os("VECTORS_VERBOSE").is_some() {
        usize::MAX
    } else {
        40
    };
    for f in failures.iter().take(shown) {
        eprintln!("{f}");
    }
    eprintln!("{} of {total} vectors passed", total - failures.len());
    assert!(failures.is_empty(), "{} vectors failed", failures.len());
}

#[test]
fn unicorn_vectors_step() {
    run_all(false);
}

#[test]
fn unicorn_vectors_block() {
    run_all(true);
}
