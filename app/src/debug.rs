//! A scriptable debugger for the guest.
//!
//! Every question about this machine used to cost a fresh boot: set one
//! `--break-at`, wait three minutes, read one number, change the flag, boot
//! again.
//!
//! A GDB stub would be the conventional answer, but there is no ARM `gdb` on
//! this machine to talk to it. This gets the same power out of a file: a list
//! of breakpoints, each with actions to run when it is hit, so one boot can
//! answer many questions and print a running commentary.
//!
//! # The script
//!
//! One breakpoint per line. Blank lines and `#` comments are ignored.
//!
//! ```text
//! # address [conditions] : actions
//! 0x00023334 slot=9            : regs, back 6, stop
//! 0x0001140c slot=9 r1=0x1f0   : regs
//! 0x02259974                   : regs, mem r0 32, count
//! ```
//!
//! Conditions, all optional and all ANDed:
//!
//! - `slot=N` — only in FCSE slot N. Every EXE in this ROM links at
//!   `0x00010000`, so without this a breakpoint fires in every process.
//! - `rN=VALUE` — only when that register holds that value.
//!
//! Actions:
//!
//! - `regs` — the register file.
//! - `mem ADDR LEN` — a hex dump. `ADDR` may be a register (`mem r0 64`
//!   follows a pointer) or a register plus an offset (`mem sp+0x18 44`).
//! - `back N` — the last N calls.
//! - `count` — how many times this breakpoint has been hit, and nothing else,
//!   for a site too hot to print at.
//! - `stop` — end the run here.
//!
//! Without `stop`, execution continues, so a script can watch a sequence
//! rather than catching one moment.

use arm::Cpu;
use gandalf::Gandalf;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Regs,
    /// `mem ADDR LEN`, where the address is a register or a literal.
    Mem(Addr, u32),
    Back(usize),
    Count,
    /// Programs and erases so far, for bracketing a call to find out whether
    /// it touches the medium at all.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Addr {
    Literal(u32),
    Register(usize),
    /// A register plus a byte offset, written `rN+0xNN`, so a struct field or
    /// a stack slot can be dumped without knowing the absolute address.
    RegisterOffset(usize, u32),
}

#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub pc: u32,
    pub slot: Option<u32>,
    /// Register conditions, all of which must hold.
    pub when: Vec<(usize, u32)>,
    pub actions: Vec<Action>,
    pub hits: u64,
}

fn parse_u32(text: &str) -> Option<u32> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        t.parse().ok().or_else(|| u32::from_str_radix(t, 16).ok())
    }
}

fn parse_register(text: &str) -> Option<usize> {
    let t = text.trim();
    match t.to_ascii_lowercase().as_str() {
        "sp" => return Some(13),
        "lr" => return Some(14),
        "pc" => return Some(15),
        _ => {}
    }
    let n = t.strip_prefix('r').or_else(|| t.strip_prefix('R'))?;
    let n: usize = n.parse().ok()?;
    (n < 16).then_some(n)
}

fn parse_addr(text: &str) -> Option<Addr> {
    if let Some((reg, off)) = text.split_once('+') {
        if let (Some(r), Some(o)) = (parse_register(reg.trim()), parse_u32(off.trim())) {
            return Some(Addr::RegisterOffset(r, o));
        }
    }
    if let Some(r) = parse_register(text) {
        return Some(Addr::Register(r));
    }
    parse_u32(text).map(Addr::Literal)
}

/// Parse a script, reporting the line number of anything that does not make
/// sense rather than quietly ignoring it.
pub fn parse(script: &str) -> Result<Vec<Breakpoint>, String> {
    let mut out = Vec::new();
    for (n, raw) in script.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |e: &str| format!("line {}: {e}", n + 1);
        let (head, tail) = match line.split_once(':') {
            Some((h, t)) => (h, t),
            None => (line, ""),
        };

        let mut words = head.split_whitespace();
        let pc = words
            .next()
            .and_then(parse_u32)
            .ok_or_else(|| at("expected an address to break on"))?;
        let mut bp = Breakpoint { pc, slot: None, when: Vec::new(), actions: Vec::new(), hits: 0 };
        for w in words {
            let (key, value) = w.split_once('=').ok_or_else(|| at(&format!("{w:?} is not key=value")))?;
            if key.eq_ignore_ascii_case("slot") {
                bp.slot = Some(parse_u32(value).ok_or_else(|| at("slot needs a number"))?);
            } else if let Some(reg) = parse_register(key) {
                bp.when.push((reg, parse_u32(value).ok_or_else(|| at("expected a value"))?));
            } else {
                return Err(at(&format!("{key:?} is not a condition")));
            }
        }

        for action in tail.split(',').map(str::trim).filter(|a| !a.is_empty()) {
            let mut parts = action.split_whitespace();
            let verb = parts.next().unwrap_or("");
            bp.actions.push(match verb {
                "regs" => Action::Regs,
                "count" => Action::Count,
                "stop" => Action::Stop,
                "back" => Action::Back(
                    parts.next().and_then(|n| n.parse().ok()).ok_or_else(|| at("back needs a count"))?,
                ),
                "mem" => {
                    let a = parts.next().and_then(parse_addr).ok_or_else(|| at("mem needs an address"))?;
                    let len = parts.next().and_then(parse_u32).unwrap_or(32);
                    Action::Mem(a, len)
                }
                other => return Err(at(&format!("{other:?} is not an action"))),
            });
        }
        if bp.actions.is_empty() {
            bp.actions.push(Action::Regs);
        }
        out.push(bp);
    }
    Ok(out)
}

/// Does this breakpoint apply right now?
pub fn matches(bp: &Breakpoint, cpu: &Cpu) -> bool {
    if cpu.r[15] != bp.pc {
        return false;
    }
    if let Some(slot) = bp.slot {
        if cpu.cp15.pid >> 25 != slot {
            return false;
        }
    }
    bp.when.iter().all(|(reg, value)| cpu.r[*reg] == *value)
}

/// Run a breakpoint's actions. Returns true if the run should end.
pub fn fire(bp: &mut Breakpoint, cpu: &mut Cpu, board: &mut Gandalf) -> bool {
    bp.hits += 1;
    let quiet = bp.actions.contains(&Action::Count);
    if !quiet {
        println!(
            "\n[break {:#010x} slot {} hit {}]",
            bp.pc,
            cpu.cp15.pid >> 25,
            bp.hits
        );
    }
    let mut stop = false;
    for action in &bp.actions {
        match action {
            Action::Count => {
                if bp.hits.is_multiple_of(1000) || bp.hits < 4 {
                    println!("[break {:#010x} hit {} times]", bp.pc, bp.hits);
                }
            }
            Action::Regs => {
                for row in 0..4 {
                    let cells: Vec<String> = (0..4)
                        .map(|c| format!("r{:<2} {:#010x}", row * 4 + c, cpu.r[row * 4 + c]))
                        .collect();
                    println!("  {}", cells.join("  "));
                }
                println!("  cpsr {:#010x}  pid {:#010x}", cpu.cpsr, cpu.cp15.pid);
            }
            Action::Back(n) => {
                let trace = cpu.call_trace();
                println!("  last {n} calls:");
                for (from, to) in trace.iter().rev().take(*n) {
                    println!("    {from:#010x} -> {to:#010x}");
                }
            }
            Action::Mem(addr, len) => {
                let base = match addr {
                    Addr::Literal(v) => *v,
                    Addr::Register(r) => cpu.r[*r],
                    Addr::RegisterOffset(r, o) => cpu.r[*r].wrapping_add(*o),
                };
                println!("  memory at {base:#010x}:");
                for row in 0..(*len).div_ceil(16) {
                    let start = base.wrapping_add(row * 16);
                    let mut bytes = Vec::new();
                    for i in 0..16.min(*len - row * 16) {
                        bytes.push(cpu.read_u8(board, start.wrapping_add(i)).ok());
                    }
                    let hex: Vec<String> = bytes
                        .iter()
                        .map(|b| b.map_or("--".into(), |v| format!("{v:02x}")))
                        .collect();
                    let txt: String = bytes
                        .iter()
                        .map(|b| match b {
                            Some(v) if (0x20..0x7f).contains(v) => *v as char,
                            Some(_) => '.',
                            None => '?',
                        })
                        .collect();
                    println!("    {start:#010x}  {:<47}  {txt}", hex.join(" "));
                }
            }
            Action::Stop => stop = true,
        }
    }
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_address_breaks_and_dumps_registers() {
        let bps = parse("0x00023334").unwrap();
        assert_eq!(bps.len(), 1);
        assert_eq!(bps[0].pc, 0x00023334);
        assert_eq!(bps[0].actions, vec![Action::Regs], "a useful default");
        assert_eq!(bps[0].slot, None);
    }

    #[test]
    fn conditions_and_actions_both_parse() {
        let bps = parse("0x1140c slot=9 r1=0x1f0 : regs, back 6, mem r0 64, stop").unwrap();
        let bp = &bps[0];
        assert_eq!(bp.pc, 0x1140c);
        assert_eq!(bp.slot, Some(9));
        assert_eq!(bp.when, vec![(1, 0x1f0)]);
        assert_eq!(
            bp.actions,
            vec![Action::Regs, Action::Back(6), Action::Mem(Addr::Register(0), 64), Action::Stop]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let bps = parse("# a note\n\n0x100 : count  # trailing\n").unwrap();
        assert_eq!(bps.len(), 1);
        assert_eq!(bps[0].actions, vec![Action::Count]);
    }

    #[test]
    fn a_mistake_names_its_line_instead_of_being_ignored() {
        assert!(parse("0x100 : regs\nnotanaddress : regs").unwrap_err().starts_with("line 2"));
        assert!(parse("0x100 wat=1 : regs").unwrap_err().contains("line 1"));
        assert!(parse("0x100 : dance").unwrap_err().contains("line 1"));
    }

    #[test]
    fn a_breakpoint_only_fires_where_every_condition_holds() {
        let bps = parse("0x1000 slot=9 r1=0x1f0 : count").unwrap();
        let mut cpu = Cpu::new();
        cpu.r[15] = 0x1000;
        cpu.r[1] = 0x1f0;
        cpu.cp15.pid = 9 << 25;
        assert!(matches(&bps[0], &cpu));

        cpu.cp15.pid = 5 << 25;
        assert!(!matches(&bps[0], &cpu), "wrong slot: every exe links at 0x10000");
        cpu.cp15.pid = 9 << 25;
        cpu.r[1] = 0;
        assert!(!matches(&bps[0], &cpu), "wrong register value");
        cpu.r[1] = 0x1f0;
        cpu.r[15] = 0x1004;
        assert!(!matches(&bps[0], &cpu), "wrong pc");
    }

    #[test]
    fn addresses_may_be_registers_so_a_pointer_can_be_followed() {
        assert_eq!(parse_addr("r7"), Some(Addr::Register(7)));
        assert_eq!(parse_addr("0x1234"), Some(Addr::Literal(0x1234)));
        assert_eq!(parse_addr("r16"), None, "no such register, and not a number either");
        assert_eq!(parse_addr("1140c"), Some(Addr::Literal(0x1140c)), "bare hex too");
    }
}
