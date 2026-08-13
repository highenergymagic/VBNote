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
/// A granule in bytes, which is what the bus is asked to vouch for.
pub const GRANULE_BYTES: u32 = 1 << TLB_GRANULE_BITS;
/// `ram_off` for a granule that is not directly-addressable RAM.
const NOT_RAM: u32 = u32::MAX;

#[derive(Copy, Clone)]
struct TlbEntry {
    /// Virtual granule number, or `u32::MAX` when empty.
    tag: u32,
    /// Physical base of the 1 KB granule.
    phys: u32,
    ap: u8,
    /// Set when the domain is a manager domain, so AP is not checked.
    manager: bool,
    /// Which accesses this granule allows: bit 0 user read, bit 1 user write,
    /// bit 2 privileged read, bit 3 privileged write.
    ///
    /// Precomputed when the entry is filled, so that a TLB hit needs no
    /// permission check at all -- matching the tag *is* the permission. That
    /// is an AND and a test in place of a chain of branches through
    /// `manager`, the S and R control bits and `ap_permits`, and it is what
    /// lets generated code perform a load inline: compiled code cannot call
    /// into a permission checker, so the check has to have happened already.
    ///
    /// Sound only because everything the mask depends on invalidates the
    /// whole TLB: `ap` and `manager` come from the tables and DACR, which
    /// set `tlb_dirty` on a write to c2 or c3, and S and R do the same on a
    /// write to c1.
    perm: u8,
    /// Byte offset of this granule in `Bus::ram()`, or `NOT_RAM`.
    ///
    /// Filled once when the entry is filled, so a hit on a RAM granule hands
    /// back an index into a slice and the physical-address dispatch never
    /// runs. Flash and every device answer `NOT_RAM` and keep the slow path.
    ram_off: u32,
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
        perm: 0,
        ram_off: NOT_RAM,
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
    translate_ram(cp15, tlb, bus, va, access, privileged, enabled).map(|(pa, _)| pa)
}

/// Translate, and also say where the access lands in `Bus::ram()` when it
/// lands in RAM at all.
///
/// The offset is the whole point: with it a load is a slice index, without it
/// the physical address has to be dispatched through the board's memory map
/// on every single access.
pub fn translate_ram<B: Bus>(
    cp15: &Cp15,
    tlb: &mut Tlb,
    bus: &mut B,
    va: u32,
    access: Access,
    privileged: bool,
    enabled: bool,
) -> Result<(u32, Option<u32>), Fault> {
    if !enabled {
        // No MMU means no TLB and no entry to have cached anything in, and
        // this is only EBOOT and the first moments of CE. Take the slow path.
        return Ok((va, None));
    }

    let vgn = va >> TLB_GRANULE_BITS;
    let slot = Tlb::slot(vgn);
    let need = need_mask(privileged, access);
    let e = tlb.entries[slot];
    if e.tag == vgn {
        if e.perm & need != 0 {
            return Ok((e.phys | (va & GRANULE_MASK), ram_at(e, va)));
        }
        return Err(denied(e, va));
    }

    let mut e = walk(cp15, bus, va)?;
    e.perm = perm_mask(
        e.ap,
        e.manager,
        cp15.control & ctl::S != 0,
        cp15.control & ctl::R != 0,
    );
    e.ram_off = bus.ram_offset(e.phys, GRANULE_BYTES).unwrap_or(NOT_RAM);
    tlb.entries[slot] = e;
    if e.perm & need != 0 {
        Ok((e.phys | (va & GRANULE_MASK), ram_at(e, va)))
    } else {
        Err(denied(e, va))
    }
}

/// Where this access lands in `Bus::ram()`, given the granule it hit.
#[inline]
fn ram_at(e: TlbEntry, va: u32) -> Option<u32> {
    if e.ram_off == NOT_RAM {
        None
    } else {
        Some(e.ram_off + (va & GRANULE_MASK))
    }
}

/// The single bit an access needs to find set in a `perm` mask.
#[inline]
fn need_mask(privileged: bool, access: Access) -> u8 {
    // Fetching is checked as a read: ARMv5 AP fields have no execute bit.
    let write = (access == Access::Write) as u8;
    1 << (((privileged as u8) << 1) | write)
}

/// Fold `ap_permits` over all four combinations, once, at fill time.
fn perm_mask(ap: u8, manager: bool, s: bool, r: bool) -> u8 {
    if manager {
        // A manager domain is not permission-checked at all.
        return 0xF;
    }
    let mut m = 0u8;
    for privileged in [false, true] {
        for write in [false, true] {
            if ap_permits(ap, privileged, write, s, r) {
                m |= 1 << (((privileged as u8) << 1) | write as u8);
            }
        }
    }
    m
}

/// The fault a denied access takes, which is the only thing the slow path
/// still needs `section` and `domain` for.
#[inline]
fn denied(e: TlbEntry, va: u32) -> Fault {
    let code = if e.section { FS_PERM_SECTION } else { FS_PERM_PAGE };
    Fault { fsr: code | ((e.domain as u32) << 4), far: va }
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
                perm: 0,
                ram_off: NOT_RAM,
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
            Ok(TlbEntry { tag, phys, ap, manager, perm: 0, ram_off: NOT_RAM, section: false, domain })
        }

        // 4 KB small page: four AP fields, one per 1 KB subpage, selected by
        // VA bits 11:10.
        2 => {
            let ap = ((d >> (4 + 2 * ((va >> 10) & 3))) & 3) as u8;
            let phys = (d & 0xFFFF_F000) | (va & 0x0000_0C00);
            Ok(TlbEntry { tag, phys, ap, manager, perm: 0, ram_off: NOT_RAM, section: false, domain })
        }

        // 1 KB tiny page: a single AP field, and exactly one granule.
        _ => {
            let ap = ((d >> 4) & 3) as u8;
            Ok(TlbEntry { tag, phys: d & 0xFFFF_FC00, ap, manager, perm: 0, ram_off: NOT_RAM, section: false, domain })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The folded mask has to answer exactly what the predicate it replaced
    /// answered, for every AP field, every domain kind and both control bits.
    ///
    /// This is the test that matters for this change: `perm_mask` runs once
    /// per TLB fill and `ap_permits` used to run once per memory access, so a
    /// disagreement would be a permission bug visible only under a guest that
    /// relies on the difference -- which this one does, at PUserKData.
    #[test]
    fn the_folded_mask_agrees_with_the_predicate() {
        for ap in 0..4u8 {
            for manager in [false, true] {
                for s in [false, true] {
                    for r in [false, true] {
                        let m = perm_mask(ap, manager, s, r);
                        for privileged in [false, true] {
                            for access in [Access::Read, Access::Write, Access::Exec] {
                                let want = manager
                                    || ap_permits(
                                        ap,
                                        privileged,
                                        access == Access::Write,
                                        s,
                                        r,
                                    );
                                let got = m & need_mask(privileged, access) != 0;
                                assert_eq!(
                                    got, want,
                                    "ap={ap} manager={manager} s={s} r={r}                                      privileged={privileged} access={access:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// A RAM granule reports an offset that names the same byte the physical
    /// address does, and a granule that is not RAM reports none.
    ///
    /// Worth a real page table rather than a unit test of the arithmetic: the
    /// fast path only engages with the MMU on, so nothing else in this crate
    /// reaches it, and a wrong offset here is silent memory corruption rather
    /// than a crash.
    #[test]
    fn a_ram_granule_hands_back_an_index_and_a_device_does_not() {
        use crate::bus::Ram;

        let mut bus = Ram::new(0, 8 * 1024 * 1024);
        let base = Cp15::default();
        let cp15 = Cp15 {
            ttbr: 0x4000,
            dacr: 1, // domain 0, client
            control: base.control | ctl::M,
            ..base
        };

        // Two 1 MB sections, AP 3 (anyone may do anything), domain 0: one
        // aimed inside the RAM, one aimed at a device address outside it.
        let section = |phys: u32| phys | (3 << 10) | 0x2;
        bus.write32(0x4000 + 4, section(0x0020_0000)); // va 0x00100000 -> RAM
        bus.write32(0x4000 + 8, section(0x4000_0000)); // va 0x00200000 -> device

        let mut tlb = Tlb::default();
        let go = |tlb: &mut Tlb, bus: &mut Ram, va| {
            translate_ram(&cp15, tlb, bus, va, Access::Read, true, true).unwrap()
        };

        let (pa, ram) = go(&mut tlb, &mut bus, 0x0010_0010);
        assert_eq!(pa, 0x0020_0010);
        assert_eq!(ram, Some(0x0020_0010), "RAM granule gives its own offset");

        // The offset must name the byte the physical address names.
        bus.write32(pa, 0xDEAD_BEEF);
        let o = ram.unwrap() as usize;
        assert_eq!(
            u32::from_le_bytes(bus.ram()[o..o + 4].try_into().unwrap()),
            0xDEAD_BEEF
        );

        let (pa, ram) = go(&mut tlb, &mut bus, 0x0020_0004);
        assert_eq!(pa, 0x4000_0004);
        assert_eq!(ram, None, "a device keeps the slow path");

        // Second time round is a TLB hit, and must answer the same.
        assert_eq!(go(&mut tlb, &mut bus, 0x0010_0010).1, Some(0x0020_0010));
        assert_eq!(go(&mut tlb, &mut bus, 0x0020_0004).1, None);
    }

    /// A granule that would run off the end of RAM is not RAM.
    #[test]
    fn a_granule_must_fit_entirely() {
        use crate::bus::Ram;
        let bus = Ram::new(0, 4096);
        assert_eq!(bus.ram_offset(3072, GRANULE_BYTES), Some(3072));
        assert_eq!(bus.ram_offset(4096 - 512, GRANULE_BYTES), None);
        assert_eq!(bus.ram_offset(8192, GRANULE_BYTES), None);
    }

    /// Fetching is checked as a read, so it must want the same bit.
    #[test]
    fn execute_is_checked_as_a_read() {
        for privileged in [false, true] {
            assert_eq!(
                need_mask(privileged, Access::Exec),
                need_mask(privileged, Access::Read)
            );
        }
    }
}
