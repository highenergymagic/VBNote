//! ARMv5 virtual memory: the two-level page table walk plus a software TLB.
//!
//! Windows CE builds a fairly ordinary v5 table — 1 MB sections for the
//! statically mapped OEMAddressTable regions and coarse tables of 4 KB pages
//! for everything the loader touches — so all four descriptor types matter.

use crate::bus::Bus;
use crate::cp15::{ctl, Cp15};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    Write,
    Exec,
}

/// A translation failure, already encoded the way CP15 c5 wants it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// Fault status: status code in bits 3:0, domain in bits 7:4.
    pub fsr: u32,
    /// Faulting virtual address (post-FCSE), for CP15 c6.
    pub far: u32,
}

// Fault status codes from the ARMv5 architecture reference.
const FS_ALIGN: u32 = 0x1;
const FS_TRANS_SECTION: u32 = 0x5;
const FS_TRANS_PAGE: u32 = 0x7;
const FS_DOMAIN_SECTION: u32 = 0x9;
const FS_DOMAIN_PAGE: u32 = 0xB;
const FS_PERM_SECTION: u32 = 0xD;
const FS_PERM_PAGE: u32 = 0xF;

// The TLB is keyed at 1 KB granularity, not 4 KB. ARMv5 small pages carry
// four independent AP fields, one per 1 KB subpage, so a 4 KB granule would
// let the permissions of whichever subpage was touched first stand in for its
// neighbours. Windows CE depends on the difference: the page holding
// PUserKData at 0xFFFFC800 is privileged-only in its lower subpages and
// user-readable in the one the shared structure lives in.
const TLB_GRANULE_BITS: u32 = 10;
const TLB_BITS: usize = 13;
const TLB_SIZE: usize = 1 << TLB_BITS;
const TLB_MASK: u32 = (TLB_SIZE - 1) as u32;
/// Offset within a TLB granule.
const GRANULE_MASK: u32 = (1 << TLB_GRANULE_BITS) - 1;

#[derive(Copy, Clone)]
struct TlbEntry {
    /// Virtual granule number, or `u32::MAX` when empty.
    tag: u32,
    /// Physical base of the 1 KB granule.
    phys: u32,
    ap: u8,
    /// Set when the domain is a manager domain, so AP is not checked.
    manager: bool,
    /// True when the descriptor came from a section, for fault reporting.
    section: bool,
    domain: u8,
}

impl TlbEntry {
    const EMPTY: TlbEntry = TlbEntry {
        tag: u32::MAX,
        phys: 0,
        ap: 0,
        manager: false,
        section: false,
        domain: 0,
    };
}

pub struct Tlb {
    entries: Box<[TlbEntry; TLB_SIZE]>,
}

impl Default for Tlb {
    fn default() -> Self {
        Tlb { entries: Box::new([TlbEntry::EMPTY; TLB_SIZE]) }
    }
}

impl Tlb {
    pub fn flush(&mut self) {
        self.entries.fill(TlbEntry::EMPTY);
    }

    #[inline]
    fn slot(vpn: u32) -> usize {
        (vpn & TLB_MASK) as usize
    }
}

/// Decide whether an access is allowed by an AP field.
#[inline]
fn ap_permits(ap: u8, privileged: bool, write: bool, s: bool, r: bool) -> bool {
    match ap & 3 {
        0 => {
            if write {
                false
            } else {
                (s && privileged) || r
            }
        }
        1 => privileged,
        2
            if write => {
                privileged
            }
        _ => true,
    }
}

/// Translate `va` for `access`, filling the TLB as a side effect.
///
/// `va` must already have had FCSE applied by the caller.
pub fn translate<B: Bus>(
    cp15: &Cp15,
    tlb: &mut Tlb,
    bus: &mut B,
    va: u32,
    access: Access,
    privileged: bool,
    enabled: bool,
) -> Result<u32, Fault> {
    if !enabled {
        return Ok(va);
    }

    let vgn = va >> TLB_GRANULE_BITS;
    let slot = Tlb::slot(vgn);
    let e = tlb.entries[slot];
    if e.tag == vgn {
        check(cp15, e, va, access, privileged)?;
        return Ok(e.phys | (va & GRANULE_MASK));
    }

    let e = walk(cp15, bus, va)?;
    tlb.entries[slot] = e;
    check(cp15, e, va, access, privileged)?;
    Ok(e.phys | (va & GRANULE_MASK))
}

#[inline]
fn check(
    cp15: &Cp15,
    e: TlbEntry,
    va: u32,
    access: Access,
    privileged: bool,
) -> Result<(), Fault> {
    if e.manager {
        return Ok(());
    }
    let write = access == Access::Write;
    let s = cp15.control & ctl::S != 0;
    let r = cp15.control & ctl::R != 0;
    if ap_permits(e.ap, privileged, write, s, r) {
        Ok(())
    } else {
        let code = if e.section { FS_PERM_SECTION } else { FS_PERM_PAGE };
        Err(Fault { fsr: code | ((e.domain as u32) << 4), far: va })
    }
}

/// Two-level hardware page table walk.
fn walk<B: Bus>(cp15: &Cp15, bus: &mut B, va: u32) -> Result<TlbEntry, Fault> {
    let l1_addr = (cp15.ttbr & 0xFFFF_C000) | ((va >> 20) << 2);
    let l1 = bus.read32(l1_addr);
    let domain = ((l1 >> 5) & 0xF) as u8;
    let dom_access = (cp15.dacr >> (2 * domain as u32)) & 3;
    let manager = dom_access == 3;

    match l1 & 3 {
        // Invalid.
        0 => Err(Fault { fsr: FS_TRANS_SECTION, far: va }),

        // Coarse second-level table: 256 entries covering 1 MB.
        1 => {
            if dom_access == 0 {
                return Err(Fault { fsr: FS_DOMAIN_PAGE | ((domain as u32) << 4), far: va });
            }
            let l2_addr = (l1 & 0xFFFF_FC00) | (((va >> 12) & 0xFF) << 2);
            l2(bus, l2_addr, va, domain, manager)
        }

        // 1 MB section.
        2 => {
            if dom_access == 0 {
                return Err(Fault { fsr: FS_DOMAIN_SECTION | ((domain as u32) << 4), far: va });
            }
            let ap = ((l1 >> 10) & 3) as u8;
            // Physical base of the 1 KB granule this VA lands in.
            let phys = (l1 & 0xFFF0_0000) | (va & 0x000F_FC00);
            Ok(TlbEntry {
                tag: va >> TLB_GRANULE_BITS,
                phys,
                ap,
                manager,
                section: true,
                domain,
            })
        }

        // Fine second-level table: 1024 entries covering 1 MB.
        _ => {
            if dom_access == 0 {
                return Err(Fault { fsr: FS_DOMAIN_PAGE | ((domain as u32) << 4), far: va });
            }
            let l2_addr = (l1 & 0xFFFF_F000) | (((va >> 10) & 0x3FF) << 2);
            l2(bus, l2_addr, va, domain, manager)
        }
    }
}

fn l2<B: Bus>(
    bus: &mut B,
    l2_addr: u32,
    va: u32,
    domain: u8,
    manager: bool,
) -> Result<TlbEntry, Fault> {
    let d = bus.read32(l2_addr);
    let tag = va >> TLB_GRANULE_BITS;
    match d & 3 {
        0 => Err(Fault { fsr: FS_TRANS_PAGE | ((domain as u32) << 4), far: va }),

        // 64 KB large page: four AP fields, selected by VA bits 15:14.
        1 => {
            let ap = ((d >> (4 + 2 * ((va >> 14) & 3))) & 3) as u8;
            let phys = (d & 0xFFFF_0000) | (va & 0x0000_FC00);
            Ok(TlbEntry { tag, phys, ap, manager, section: false, domain })
        }

        // 4 KB small page: four AP fields, one per 1 KB subpage, selected by
        // VA bits 11:10.
        2 => {
            let ap = ((d >> (4 + 2 * ((va >> 10) & 3))) & 3) as u8;
            let phys = (d & 0xFFFF_F000) | (va & 0x0000_0C00);
            Ok(TlbEntry { tag, phys, ap, manager, section: false, domain })
        }

        // 1 KB tiny page: a single AP field, and exactly one granule.
        _ => {
            let ap = ((d >> 4) & 3) as u8;
            Ok(TlbEntry { tag, phys: d & 0xFFFF_FC00, ap, manager, section: false, domain })
        }
    }
}

/// Re-walk `va` and describe what the tables say, for diagnostics. Never
/// called on the hot path.
pub fn explain<B: Bus>(cp15: &Cp15, bus: &mut B, va: u32) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let s = cp15.control & ctl::S != 0;
    let r = cp15.control & ctl::R != 0;
    let _ = write!(
        out,
        "va {va:#010x}  ttbr {:#010x}  dacr {:#010x}  S={} R={}",
        cp15.ttbr, cp15.dacr, s as u8, r as u8
    );
    if !cp15.mmu_enabled() {
        let _ = write!(out, "
    MMU disabled");
        return out;
    }
    let l1_addr = (cp15.ttbr & 0xFFFF_C000) | ((va >> 20) << 2);
    let l1 = bus.read32(l1_addr);
    let domain = (l1 >> 5) & 0xF;
    let dom_access = (cp15.dacr >> (2 * domain)) & 3;
    let kind = match l1 & 3 {
        0 => "fault",
        1 => "coarse table",
        2 => "section",
        _ => "fine table",
    };
    let _ = write!(
        out,
        "
    L1 @{l1_addr:#010x} = {l1:#010x}  ({kind}, domain {domain}, dacr {})",
        match dom_access {
            0 => "no-access",
            1 => "client",
            2 => "reserved",
            _ => "manager",
        }
    );
    match l1 & 3 {
        2 => {
            let ap = (l1 >> 10) & 3;
            let _ = write!(
                out,
                "
    AP={ap:02b} -> priv {} user {}",
                access_str(ap as u8, true, s, r),
                access_str(ap as u8, false, s, r)
            );
        }
        1 | 3 => {
            let l2_addr = if l1 & 3 == 1 {
                (l1 & 0xFFFF_FC00) | (((va >> 12) & 0xFF) << 2)
            } else {
                (l1 & 0xFFFF_F000) | (((va >> 10) & 0x3FF) << 2)
            };
            let d = bus.read32(l2_addr);
            let page = match d & 3 {
                0 => "fault",
                1 => "large 64K",
                2 => "small 4K",
                _ => "tiny 1K",
            };
            let ap = match d & 3 {
                1 => (d >> (4 + 2 * ((va >> 14) & 3))) & 3,
                2 => (d >> (4 + 2 * ((va >> 10) & 3))) & 3,
                _ => (d >> 4) & 3,
            };
            let _ = write!(out, "
    L2 @{l2_addr:#010x} = {d:#010x}  ({page})");
            if d & 3 != 0 {
                let _ = write!(
                    out,
                    "
    AP={ap:02b} -> priv {} user {}",
                    access_str(ap as u8, true, s, r),
                    access_str(ap as u8, false, s, r)
                );
            }
        }
        _ => {}
    }
    out
}

fn access_str(ap: u8, privileged: bool, s: bool, r: bool) -> &'static str {
    let can_read = ap_permits(ap, privileged, false, s, r);
    let can_write = ap_permits(ap, privileged, true, s, r);
    match (can_read, can_write) {
        (true, true) => "rw",
        (true, false) => "ro",
        (false, false) => "--",
        (false, true) => "w-",
    }
}

/// Build the fault a misaligned access raises when CP15 c1[1] is set.
pub fn alignment_fault(va: u32) -> Fault {
    Fault { fsr: FS_ALIGN, far: va }
}
