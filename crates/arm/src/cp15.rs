//! CP15, the ARMv5 system control coprocessor, as implemented by the XScale
//! core in the PXA270.
//!
//! Windows CE leans on two things here that a naive ARM core gets wrong:
//! the FCSE process ID in c13 (CE's "slot" model relocates every process's
//! low 32 MB through it) and high exception vectors selected by c1[13].

/// c1 control register bits we act on.
pub mod ctl {
    pub const M: u32 = 1 << 0; // MMU enable
    pub const A: u32 = 1 << 1; // alignment fault enable
    pub const C: u32 = 1 << 2; // data cache enable
    pub const W: u32 = 1 << 3; // write buffer enable
    pub const S: u32 = 1 << 8; // system protection
    pub const R: u32 = 1 << 9; // ROM protection
    pub const I: u32 = 1 << 12; // instruction cache enable
    pub const V: u32 = 1 << 13; // high exception vectors at 0xFFFF0000
}

#[derive(Debug, Clone)]
pub struct Cp15 {
    /// c0,c0,0 main ID.
    pub id: u32,
    /// c0,c0,1 cache type.
    pub cache_type: u32,
    /// c1,c0,0 control.
    pub control: u32,
    /// c1,c0,1 auxiliary control (XScale).
    pub aux_control: u32,
    /// c2,c0,0 translation table base.
    pub ttbr: u32,
    /// c3,c0,0 domain access control.
    pub dacr: u32,
    /// c5,c0,0 data fault status.
    pub fsr: u32,
    /// c5,c0,1 instruction fault status.
    pub ifsr: u32,
    /// c6,c0,0 fault address.
    pub far: u32,
    /// c13,c0,0 FCSE process ID. Only bits 31:25 are meaningful.
    pub pid: u32,
    /// c15 XScale coprocessor access rights.
    pub cpar: u32,
    /// Set whenever a write invalidates cached translations.
    pub tlb_dirty: bool,
    /// Set when the M bit has been written but the change has not yet taken
    /// effect. See `Cpu::mmu_active`.
    pub mmu_change_pending: bool,
    /// Safety net: apply a pending change after this many instructions even
    /// if no branch arrives.
    pub mmu_change_deadline: u8,
}

impl Default for Cp15 {
    fn default() -> Self {
        Cp15 {
            // PXA270 (XScale core generation 3, "Bulverde"). The OAL reads
            // this to pick its CPU-specific paths, so it has to look right.
            id: 0x6905_9153,
            // 32 KB data / 32 KB instruction, 32-byte lines, 32-way.
            cache_type: 0x0B16_1B16,
            control: 0x0000_0078,
            aux_control: 0,
            ttbr: 0,
            dacr: 0,
            fsr: 0,
            ifsr: 0,
            far: 0,
            pid: 0,
            cpar: 0,
            tlb_dirty: false,
            mmu_change_pending: false,
            mmu_change_deadline: 0,
        }
    }
}

impl Cp15 {
    #[inline]
    pub fn mmu_enabled(&self) -> bool {
        self.control & ctl::M != 0
    }

    #[inline]
    pub fn alignment_faults(&self) -> bool {
        self.control & ctl::A != 0
    }

    #[inline]
    pub fn high_vectors(&self) -> bool {
        self.control & ctl::V != 0
    }

    /// Apply the FCSE mapping. Addresses in the bottom 32 MB are relocated
    /// into the current process's slot; everything above is untouched.
    #[inline]
    pub fn fcse(&self, va: u32) -> u32 {
        if va < 0x0200_0000 {
            va | (self.pid & 0xFE00_0000)
        } else {
            va
        }
    }

    pub fn read(&self, crn: u32, crm: u32, op1: u32, op2: u32) -> u32 {
        let _ = op1;
        match (crn, crm, op2) {
            (0, 0, 0) => self.id,
            (0, 0, 1) => self.cache_type,
            (0, 0, _) => self.id,
            (1, 0, 0) => self.control,
            (1, 0, 1) => self.aux_control,
            (2, 0, _) => self.ttbr,
            (3, 0, _) => self.dacr,
            (5, 0, 0) => self.fsr,
            (5, 0, 1) => self.ifsr,
            (6, 0, _) => self.far,
            (13, 0, _) => self.pid,
            (15, 1, _) => self.cpar,
            _ => 0,
        }
    }

    pub fn write(&mut self, crn: u32, crm: u32, op1: u32, op2: u32, val: u32) {
        let _ = op1;
        match (crn, crm, op2) {
            (1, 0, 0) => {
                let changed = self.control ^ val;
                self.control = val;
                // Turning the MMU on or off, or changing S/R, changes how
                // every existing translation resolves.
                if changed & (ctl::M | ctl::S | ctl::R) != 0 {
                    self.tlb_dirty = true;
                }
                // An MMU enable or disable does not take effect immediately:
                // instructions already in the pipeline complete under the old
                // regime, and the change becomes visible at the next pipeline
                // flush. Firmware depends on this, and both of EBOOT's
                // sequences are written for it:
                //
                //   enable:  mcr c1 (on) ; bx r2      -> r2 is a virtual address
                //   disable: mcr c1 (off); str; mov ; mov pc, r2
                //                                     -> r2 is a physical address
                //
                // In both, the instructions between the MCR and the branch
                // must run under the old mapping, and the branch target must
                // be fetched under the new one. Modelling it as "applies at
                // the next branch" satisfies both; a fixed instruction count
                // cannot, because the two sequences have different lengths.
                if changed & ctl::M != 0 {
                    self.mmu_change_pending = true;
                    self.mmu_change_deadline = 8;
                }
            }
            (1, 0, 1) => self.aux_control = val,
            (2, 0, _) => {
                self.ttbr = val;
                self.tlb_dirty = true;
            }
            (3, 0, _) => {
                self.dacr = val;
                self.tlb_dirty = true;
            }
            (5, 0, 0) => self.fsr = val,
            (5, 0, 1) => self.ifsr = val,
            (6, 0, _) => self.far = val,
            // c7 cache maintenance: we have no caches to maintain. Note that
            // c7,c10,4 (drain write buffer) and c7,c5,0 (flush I-cache) are
            // used by the OAL around self-modifying code; being a no-op is
            // correct for us because stores are immediately visible.
            (7, ..) => {}
            // c8 TLB maintenance. We flush everything for any of them.
            (8, ..) => self.tlb_dirty = true,
            (9, ..) | (10, ..) => {} // cache / TLB lockdown
            (13, 0, _) => {
                self.pid = val;
                self.tlb_dirty = true;
            }
            (15, 1, _) => self.cpar = val,
            _ => {}
        }
    }
}
