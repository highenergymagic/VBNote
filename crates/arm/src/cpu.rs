//! ARMv5TE core state: registers, banking, exceptions, and the memory
//! accessors that every instruction goes through.

use crate::bus::Bus;
use crate::cp15::Cp15;
use crate::mmu::{self, Access, Fault, Tlb};

pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1B;
pub const MODE_SYS: u32 = 0x1F;

// CPSR bits.
pub const N_BIT: u32 = 1 << 31;
pub const Z_BIT: u32 = 1 << 30;
pub const C_BIT: u32 = 1 << 29;
pub const V_BIT: u32 = 1 << 28;
pub const Q_BIT: u32 = 1 << 27;
pub const I_BIT: u32 = 1 << 7;
pub const F_BIT: u32 = 1 << 6;
pub const T_BIT: u32 = 1 << 5;
pub const MODE_MASK: u32 = 0x1F;

/// Bank index for the mode-private copies of r13/r14 and SPSR.
/// User and System share a bank.
#[inline]
fn bank(mode: u32) -> usize {
    match mode {
        MODE_FIQ => 0,
        MODE_IRQ => 1,
        MODE_SVC => 2,
        MODE_ABT => 3,
        MODE_UND => 4,
        _ => 5, // USR and SYS
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Exception {
    Reset,
    Undefined,
    Swi,
    PrefetchAbort,
    DataAbort,
    Irq,
    Fiq,
}

impl Exception {
    fn vector_offset(self) -> u32 {
        match self {
            Exception::Reset => 0x00,
            Exception::Undefined => 0x04,
            Exception::Swi => 0x08,
            Exception::PrefetchAbort => 0x0C,
            Exception::DataAbort => 0x10,
            Exception::Irq => 0x18,
            Exception::Fiq => 0x1C,
        }
    }

    fn target_mode(self) -> u32 {
        match self {
            Exception::Reset | Exception::Swi => MODE_SVC,
            Exception::Undefined => MODE_UND,
            Exception::PrefetchAbort | Exception::DataAbort => MODE_ABT,
            Exception::Irq => MODE_IRQ,
            Exception::Fiq => MODE_FIQ,
        }
    }
}

pub struct Cpu {
    /// The live register file. r15 is the address of the instruction being
    /// executed plus 8 (ARM) or 4 (Thumb) while an instruction runs, matching
    /// the architectural prefetch offset.
    pub r: [u32; 16],
    pub cpsr: u32,
    /// SPSR of the current mode. Meaningless in User and System mode.
    pub spsr: u32,

    banked_r13_14: [[u32; 2]; 6],
    banked_spsr: [u32; 6],
    /// r8..r12 for User/System/etc, live whenever the mode is not FIQ.
    banked_usr_r8_12: [u32; 5],
    /// r8..r12 for FIQ, live whenever the mode is FIQ.
    banked_fiq_r8_12: [u32; 5],

    pub cp15: Cp15,
    pub tlb: Tlb,

    /// Address of the instruction currently executing.
    pub insn_addr: u32,
    /// Set when an instruction wrote r15, so the normal advance is skipped.
    pub branched: bool,
    /// Set by WFI-style idle loops; cleared when an interrupt arrives.
    pub halted: bool,
    /// XScale CP14 c7 power mode: 0 runs, 1 idles, 3 and above sleep.
    pub pwrmode: u8,
    /// Set when the guest requested sleep rather than idle. Nothing we model
    /// is a wake source for it, so the core stays down and the host can say
    /// so instead of pretending to run.
    pub suspended: bool,
    /// Forces the next translation to use User privilege, for LDRT/STRT.
    pub force_user: bool,
    /// A value loaded by LDR that must not land until base writeback is done.
    pub pending_load: Option<(usize, u32)>,
    /// Whether address translation is actually in force. This lags
    /// `cp15.control`'s M bit by a couple of instructions, because that is
    /// what real cores do and what firmware depends on when it enables the
    /// MMU and immediately branches to a virtual address.
    pub mmu_active: bool,
    /// The instruction word currently executing, kept for diagnostics.
    pub current_insn: u32,
    /// Address and encoding of undefined instructions, for bring-up. Bounded
    /// so a runaway guest cannot exhaust memory.
    pub undefined_log: Vec<(u32, u32)>,
    /// Count of each exception taken, indexed by `Exception as usize`.
    pub exception_counts: [u64; 7],
    /// Ring of the most recent calls as (call site, target). The single most
    /// useful bring-up tool: when the guest parks in a leaf function, this
    /// says who put it there.
    pub call_ring: Box<[(u32, u32); Cpu::CALL_RING]>,
    pub call_at: usize,
    /// Distinct data aborts, as (pc, faulting va, fsr, mode). Bounded.
    pub data_abort_log: Vec<(u32, u32, u32, u32)>,

    /// Half-open range of virtual addresses to notice data reads of.
    ///
    /// Reaching a piece of data is often easier than reaching the code that
    /// wants it: a string's address is known from the ROM, while the code
    /// that loads it may build its identifier from immediates that no search
    /// of the binary will find.
    pub watch: Option<(u32, u32)>,
    /// Accesses inside `watch`, as (va, pc, FCSE pid, is a write). Bounded.
    pub watch_hits: Vec<(u32, u32, u32, bool)>,

    pub cycles: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    pub const CALL_RING: usize = 1 << 16;

    /// Record a call for the trace ring.
    #[inline]
    pub fn record_call(&mut self, from: u32, to: u32) {
        self.call_ring[self.call_at % Self::CALL_RING] = (from, to);
        self.call_at += 1;
    }

    /// The recorded calls, oldest first.
    pub fn call_trace(&self) -> Vec<(u32, u32)> {
        let n = self.call_at.min(Self::CALL_RING);
        let start = self.call_at - n;
        (start..self.call_at).map(|i| self.call_ring[i % Self::CALL_RING]).collect()
    }

    pub fn new() -> Self {
        let mut cpu = Cpu {
            r: [0; 16],
            cpsr: MODE_SVC | I_BIT | F_BIT,
            spsr: 0,
            banked_r13_14: [[0; 2]; 6],
            banked_spsr: [0; 6],
            banked_usr_r8_12: [0; 5],
            banked_fiq_r8_12: [0; 5],
            cp15: Cp15::default(),
            tlb: Tlb::default(),
            insn_addr: 0,
            branched: false,
            halted: false,
            pwrmode: 0,
            suspended: false,
            force_user: false,
            pending_load: None,
            mmu_active: false,
            current_insn: 0,
            undefined_log: Vec::new(),
            exception_counts: [0; 7],
            call_ring: Box::new([(0, 0); Cpu::CALL_RING]),
            call_at: 0,
            data_abort_log: Vec::new(),
            watch: None,
            watch_hits: Vec::new(),
            cycles: 0,
        };
        cpu.r[15] = 0;
        cpu
    }

    // ---- flags -----------------------------------------------------------

    #[inline]
    pub fn n(&self) -> bool {
        self.cpsr & N_BIT != 0
    }
    #[inline]
    pub fn z(&self) -> bool {
        self.cpsr & Z_BIT != 0
    }
    #[inline]
    pub fn c(&self) -> bool {
        self.cpsr & C_BIT != 0
    }
    #[inline]
    pub fn v(&self) -> bool {
        self.cpsr & V_BIT != 0
    }
    #[inline]
    pub fn thumb(&self) -> bool {
        self.cpsr & T_BIT != 0
    }
    #[inline]
    pub fn mode(&self) -> u32 {
        self.cpsr & MODE_MASK
    }
    #[inline]
    pub fn privileged(&self) -> bool {
        self.mode() != MODE_USR
    }

    #[inline]
    pub fn set_flag(&mut self, bit: u32, on: bool) {
        if on {
            self.cpsr |= bit;
        } else {
            self.cpsr &= !bit;
        }
    }

    #[inline]
    pub fn set_nz(&mut self, val: u32) {
        self.set_flag(N_BIT, val & 0x8000_0000 != 0);
        self.set_flag(Z_BIT, val == 0);
    }

    /// Evaluate an ARM condition code.
    #[inline]
    pub fn cond_passes(&self, cond: u32) -> bool {
        match cond {
            0x0 => self.z(),
            0x1 => !self.z(),
            0x2 => self.c(),
            0x3 => !self.c(),
            0x4 => self.n(),
            0x5 => !self.n(),
            0x6 => self.v(),
            0x7 => !self.v(),
            0x8 => self.c() && !self.z(),
            0x9 => !self.c() || self.z(),
            0xA => self.n() == self.v(),
            0xB => self.n() != self.v(),
            0xC => !self.z() && self.n() == self.v(),
            0xD => self.z() || self.n() != self.v(),
            _ => true, // 0xE always; 0xF is unconditional space on v5
        }
    }

    // ---- register banking ------------------------------------------------

    /// Switch to `new_mode`, spilling and filling the banked registers.
    pub fn set_mode(&mut self, new_mode: u32) {
        let old = self.mode();
        if old == new_mode {
            return;
        }
        let (oi, ni) = (bank(old), bank(new_mode));

        // FIQ banks r8..r12 as well as r13/r14.
        if (old == MODE_FIQ) != (new_mode == MODE_FIQ) {
            if old == MODE_FIQ {
                self.banked_fiq_r8_12.copy_from_slice(&self.r[8..13]);
                self.r[8..13].copy_from_slice(&self.banked_usr_r8_12);
            } else {
                self.banked_usr_r8_12.copy_from_slice(&self.r[8..13]);
                self.r[8..13].copy_from_slice(&self.banked_fiq_r8_12);
            }
        }

        if oi != ni {
            self.banked_r13_14[oi] = [self.r[13], self.r[14]];
            self.banked_spsr[oi] = self.spsr;
            self.r[13] = self.banked_r13_14[ni][0];
            self.r[14] = self.banked_r13_14[ni][1];
            self.spsr = self.banked_spsr[ni];
        }

        self.cpsr = (self.cpsr & !MODE_MASK) | new_mode;
    }

    /// Write CPSR wholesale, handling any implied mode switch.
    pub fn write_cpsr(&mut self, val: u32) {
        let new_mode = val & MODE_MASK;
        if new_mode != self.mode() {
            self.set_mode(new_mode);
        }
        self.cpsr = val | 0x10; // bit 4 reads as 1 on ARMv5
    }

    /// Read a register with r15 already carrying its prefetch offset.
    #[inline]
    pub fn reg(&self, i: usize) -> u32 {
        self.r[i]
    }

    #[inline]
    pub fn set_reg(&mut self, i: usize, val: u32) {
        self.r[i] = val;
        if i == 15 {
            self.branched = true;
        }
    }

    /// Read a register as seen from User mode, for LDM/STM with the ^ suffix
    /// and for MRS/MSR on the User bank.
    pub fn user_reg(&self, i: usize) -> u32 {
        match (self.mode(), i) {
            (MODE_FIQ, 8..=12) => self.banked_usr_r8_12[i - 8],
            (MODE_USR | MODE_SYS, _) => self.r[i],
            (_, 13 | 14) => self.banked_r13_14[bank(MODE_USR)][i - 13],
            _ => self.r[i],
        }
    }

    pub fn set_user_reg(&mut self, i: usize, val: u32) {
        match (self.mode(), i) {
            (MODE_FIQ, 8..=12) => self.banked_usr_r8_12[i - 8] = val,
            (MODE_USR | MODE_SYS, _) => self.r[i] = val,
            (_, 13 | 14) => self.banked_r13_14[bank(MODE_USR)][i - 13] = val,
            _ => self.r[i] = val,
        }
    }

    // ---- branching -------------------------------------------------------

    /// Plain branch: stays in the current instruction set.
    #[inline]
    pub fn branch(&mut self, addr: u32) {
        let mask = if self.thumb() { !1 } else { !3 };
        self.r[15] = addr & mask;
        self.branched = true;
    }

    /// Interworking branch: bit 0 selects Thumb.
    #[inline]
    pub fn branch_exchange(&mut self, addr: u32) {
        if addr & 1 != 0 {
            self.cpsr |= T_BIT;
            self.r[15] = addr & !1;
        } else {
            self.cpsr &= !T_BIT;
            self.r[15] = addr & !3;
        }
        self.branched = true;
    }

    // ---- exceptions ------------------------------------------------------

    #[inline]
    fn vector_base(&self) -> u32 {
        if self.cp15.high_vectors() {
            0xFFFF_0000
        } else {
            0x0000_0000
        }
    }

    /// Enter an exception. `lr` is the value to place in the target mode's
    /// r14, already adjusted for the architectural return offset.
    pub fn enter_exception(&mut self, exc: Exception, lr: u32) {
        self.exception_counts[exc as usize] += 1;
        let old_cpsr = self.cpsr;
        let mode = exc.target_mode();
        self.set_mode(mode);
        self.spsr = old_cpsr;
        self.r[14] = lr;

        self.cpsr &= !T_BIT; // exceptions always land in ARM state
        self.cpsr |= I_BIT;
        if matches!(exc, Exception::Reset | Exception::Fiq) {
            self.cpsr |= F_BIT;
        }

        self.r[15] = self.vector_base() + exc.vector_offset();
        self.branched = true;
        self.halted = false;
    }

    /// Take a data abort for `fault`, raised by the instruction at
    /// `self.insn_addr`.
    pub fn data_abort(&mut self, fault: Fault) {
        if self.data_abort_log.len() < 32 {
            let e = (self.insn_addr, fault.far, fault.fsr, self.mode());
            if !self.data_abort_log.contains(&e) {
                self.data_abort_log.push(e);
            }
        }
        self.cp15.fsr = fault.fsr;
        self.cp15.far = fault.far;
        self.enter_exception(Exception::DataAbort, self.insn_addr.wrapping_add(8));
    }

    pub fn prefetch_abort(&mut self, fault: Fault) {
        self.cp15.ifsr = fault.fsr;
        self.enter_exception(Exception::PrefetchAbort, self.insn_addr.wrapping_add(4));
    }

    // ---- memory ----------------------------------------------------------

    #[inline]
    fn translate<B: Bus>(
        &mut self,
        bus: &mut B,
        va: u32,
        access: Access,
    ) -> Result<u32, Fault> {
        if access != Access::Exec {
            if let Some((lo, hi)) = self.watch {
                if (lo..hi).contains(&va) && self.watch_hits.len() < 64 {
                    let write = access == Access::Write;
                    self.watch_hits.push((va, self.r[15], self.cp15.pid, write));
                }
            }
        }
        let mva = self.cp15.fcse(va);
        let priv_mode = self.privileged() && !self.force_user;
        let enabled = self.mmu_active;
        mmu::translate(&self.cp15, &mut self.tlb, bus, mva, access, priv_mode, enabled)
    }

    /// `translate`, and also where the access lands in `bus.ram()`.
    #[inline]
    fn translate_ram<B: Bus>(
        &mut self,
        bus: &mut B,
        va: u32,
        access: Access,
    ) -> Result<(u32, Option<u32>), Fault> {
        if access != Access::Exec {
            if let Some((lo, hi)) = self.watch {
                if (lo..hi).contains(&va) && self.watch_hits.len() < 64 {
                    let write = access == Access::Write;
                    self.watch_hits.push((va, self.r[15], self.cp15.pid, write));
                }
            }
        }
        let mva = self.cp15.fcse(va);
        let priv_mode = self.privileged() && !self.force_user;
        let enabled = self.mmu_active;
        mmu::translate_ram(&self.cp15, &mut self.tlb, bus, mva, access, priv_mode, enabled)
    }

    pub fn read_u8<B: Bus>(&mut self, bus: &mut B, va: u32) -> Result<u8, Fault> {
        let (pa, ram) = self.translate_ram(bus, va, Access::Read)?;
        match ram {
            Some(o) => Ok(bus.ram()[o as usize]),
            None => Ok(bus.read8(pa)),
        }
    }

    pub fn read_u16<B: Bus>(&mut self, bus: &mut B, va: u32) -> Result<u16, Fault> {
        if va & 1 != 0 && self.cp15.alignment_faults() {
            return Err(mmu::alignment_fault(va));
        }
        let (pa, ram) = self.translate_ram(bus, va & !1, Access::Read)?;
        match ram {
            Some(o) => {
                let o = o as usize;
                Ok(u16::from_le_bytes(bus.ram()[o..o + 2].try_into().unwrap()))
            }
            None => Ok(bus.read16(pa)),
        }
    }

    /// Word load. An unaligned address rotates the loaded value, which is
    /// what ARMv5 does when alignment faults are off.
    pub fn read_u32<B: Bus>(&mut self, bus: &mut B, va: u32) -> Result<u32, Fault> {
        if va & 3 != 0 && self.cp15.alignment_faults() {
            return Err(mmu::alignment_fault(va));
        }
        let (pa, ram) = self.translate_ram(bus, va & !3, Access::Read)?;
        let val = match ram {
            Some(o) => {
                let o = o as usize;
                u32::from_le_bytes(bus.ram()[o..o + 4].try_into().unwrap())
            }
            None => bus.read32(pa),
        };
        Ok(val.rotate_right(8 * (va & 3)))
    }

    /// Word load without the rotate, for instruction fetch and LDM.
    pub fn read_u32_aligned<B: Bus>(&mut self, bus: &mut B, va: u32) -> Result<u32, Fault> {
        let (pa, ram) = self.translate_ram(bus, va & !3, Access::Read)?;
        match ram {
            Some(o) => {
                let o = o as usize;
                Ok(u32::from_le_bytes(bus.ram()[o..o + 4].try_into().unwrap()))
            }
            None => Ok(bus.read32(pa)),
        }
    }

    pub fn write_u8<B: Bus>(&mut self, bus: &mut B, va: u32, val: u8) -> Result<(), Fault> {
        let (pa, ram) = self.translate_ram(bus, va, Access::Write)?;
        match ram {
            Some(o) => bus.ram_mut()[o as usize] = val,
            None => bus.write8(pa, val),
        }
        Ok(())
    }

    pub fn write_u16<B: Bus>(&mut self, bus: &mut B, va: u32, val: u16) -> Result<(), Fault> {
        if va & 1 != 0 && self.cp15.alignment_faults() {
            return Err(mmu::alignment_fault(va));
        }
        let (pa, ram) = self.translate_ram(bus, va & !1, Access::Write)?;
        match ram {
            Some(o) => {
                let o = o as usize;
                bus.ram_mut()[o..o + 2].copy_from_slice(&val.to_le_bytes());
            }
            None => bus.write16(pa, val),
        }
        Ok(())
    }

    pub fn write_u32<B: Bus>(&mut self, bus: &mut B, va: u32, val: u32) -> Result<(), Fault> {
        if va & 3 != 0 && self.cp15.alignment_faults() {
            return Err(mmu::alignment_fault(va));
        }
        let (pa, ram) = self.translate_ram(bus, va & !3, Access::Write)?;
        match ram {
            Some(o) => {
                let o = o as usize;
                bus.ram_mut()[o..o + 4].copy_from_slice(&val.to_le_bytes());
            }
            None => bus.write32(pa, val),
        }
        Ok(())
    }

    fn fetch_u32<B: Bus>(&mut self, bus: &mut B, va: u32) -> Result<u32, Fault> {
        let pa = self.translate(bus, va & !3, Access::Exec)?;
        Ok(bus.fetch32(pa))
    }

    fn fetch_u16<B: Bus>(&mut self, bus: &mut B, va: u32) -> Result<u16, Fault> {
        let pa = self.translate(bus, va & !1, Access::Exec)?;
        Ok(bus.fetch16(pa))
    }

    // ---- execution -------------------------------------------------------

    /// Execute one instruction, or take a pending interrupt.
    ///
    /// Returns the number of cycles consumed.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.cp15.tlb_dirty {
            self.tlb.flush();
            self.cp15.tlb_dirty = false;
        }


        // Interrupts are sampled between instructions.
        if bus.fiq_pending() && self.cpsr & F_BIT == 0 {
            let lr = self.r[15].wrapping_add(4);
            self.enter_exception(Exception::Fiq, lr);
            return 3;
        }
        if bus.irq_pending() && self.cpsr & I_BIT == 0 {
            let lr = self.r[15].wrapping_add(4);
            self.enter_exception(Exception::Irq, lr);
            return 3;
        }
        if self.halted {
            // Waking from idle or sleep is independent of the CPSR mask: the
            // core restarts when the interrupt controller asserts, and the
            // exception itself is only taken if CPSR allows it. Windows CE
            // idles with interrupts masked and relies on this.
            if !self.suspended && (bus.irq_pending() || bus.fiq_pending()) {
                self.halted = false;
                self.pwrmode = 0;
            } else {
                return 1;
            }
        }

        let addr = self.r[15];
        self.insn_addr = addr;
        self.branched = false;

        let cycles = if self.thumb() {
            match self.fetch_u16(bus, addr) {
                Ok(insn) => {
                    self.current_insn = insn as u32;
                    self.r[15] = addr.wrapping_add(4);
                    let c = self.execute_thumb(bus, insn);
                    if !self.branched {
                        self.r[15] = addr.wrapping_add(2);
                    }
                    c
                }
                Err(f) => {
                    self.prefetch_abort(f);
                    3
                }
            }
        } else {
            match self.fetch_u32(bus, addr) {
                Ok(insn) => {
                    self.current_insn = insn;
                    self.r[15] = addr.wrapping_add(8);
                    let c = self.execute_arm(bus, insn);
                    if !self.branched {
                        self.r[15] = addr.wrapping_add(4);
                    }
                    c
                }
                Err(f) => {
                    self.prefetch_abort(f);
                    3
                }
            }
        };

        // A pending MMU enable or disable becomes visible at the next
        // pipeline flush, so the branch target is fetched under the new
        // mapping while everything before it ran under the old one.
        if self.cp15.mmu_change_pending {
            self.cp15.mmu_change_deadline = self.cp15.mmu_change_deadline.saturating_sub(1);
            if self.branched || self.cp15.mmu_change_deadline == 0 {
                self.cp15.mmu_change_pending = false;
                self.mmu_active = self.cp15.mmu_enabled();
                self.tlb.flush();
            }
        }

        self.cycles += cycles as u64;
        cycles
    }

    /// Run until `budget` cycles have been consumed, ticking the bus as we go.
    pub fn run<B: Bus>(&mut self, bus: &mut B, budget: u32) -> u32 {
        let mut spent = 0;
        while spent < budget {
            let c = self.step(bus);
            bus.tick(c);
            spent += c;
        }
        spent
    }
}
