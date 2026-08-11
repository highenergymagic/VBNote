//! ARM-state instruction execution for ARMv5TE.

use crate::alu::*;
use crate::bus::Bus;
use crate::cpu::*;
use crate::mmu::Fault;

/// XScale CP14 c7 power modes. Idle merely stops the clock; sleep and below
/// power the core down.
pub const PWRMODE_IDLE: u8 = 1;
pub const PWRMODE_SLEEP: u8 = 3;

impl Cpu {
    /// Read a register for an operand. `pc_extra` is added when the register
    /// is r15, because a register-specified shift reads PC as insn+12 where
    /// everything else reads insn+8.
    #[inline]
    fn op_reg(&self, i: usize, pc_extra: u32) -> u32 {
        if i == 15 {
            self.r[15].wrapping_add(pc_extra)
        } else {
            self.r[i]
        }
    }

    pub fn execute_arm<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let cond = insn >> 28;
        if cond == 0xF {
            return self.execute_unconditional(insn);
        }
        if !self.cond_passes(cond) {
            return 1;
        }

        // Order matters: the 000 encoding space overlaps heavily, so the
        // narrower patterns have to be tested before data processing.
        if insn & 0x0FFF_FFF0 == 0x012F_FF10 {
            let target = self.r[(insn & 0xF) as usize];
            self.branch_exchange(target);
            return 3;
        }
        if insn & 0x0FFF_FFF0 == 0x012F_FF30 {
            // BLX (register)
            let target = self.r[(insn & 0xF) as usize];
            self.record_call(self.insn_addr, target);
            self.r[14] = self.insn_addr.wrapping_add(4);
            self.branch_exchange(target);
            return 3;
        }
        if insn & 0x0FFF_0FF0 == 0x016F_0F10 {
            // CLZ
            let rd = ((insn >> 12) & 0xF) as usize;
            let rm = self.r[(insn & 0xF) as usize];
            self.set_reg(rd, rm.leading_zeros());
            return 1;
        }
        if insn & 0x0FB0_0FF0 == 0x0100_0090 {
            return self.exec_swap(bus, insn);
        }
        if insn & 0x0FC0_00F0 == 0x0000_0090 {
            return self.exec_multiply(insn);
        }
        if insn & 0x0F80_00F0 == 0x0080_0090 {
            return self.exec_multiply_long(insn);
        }
        if insn & 0x0F90_0FF0 == 0x0100_0050 {
            return self.exec_saturating(insn);
        }
        if insn & 0x0F90_0090 == 0x0100_0080 {
            return self.exec_signed_multiply(insn);
        }
        if insn & 0x0E00_0090 == 0x0000_0090 && insn & 0x60 != 0 {
            return self.exec_extra_transfer(bus, insn);
        }
        if insn & 0x0FBF_0FFF == 0x010F_0000 {
            // MRS
            let rd = ((insn >> 12) & 0xF) as usize;
            let val = if insn & (1 << 22) != 0 { self.spsr } else { self.cpsr };
            self.set_reg(rd, val);
            return 1;
        }
        if insn & 0x0FB0_FFF0 == 0x0120_F000 || insn & 0x0FB0_F000 == 0x0320_F000 {
            return self.exec_msr(insn);
        }

        match (insn >> 25) & 7 {
            0 | 1 => self.exec_data_processing(insn),
            2 | 3 => self.exec_single_transfer(bus, insn),
            4 => self.exec_block_transfer(bus, insn),
            5 => {
                // B / BL
                let offset = ((insn << 8) as i32 >> 6) as u32;
                let target = self.r[15].wrapping_add(offset);
                if insn & (1 << 24) != 0 {
                    self.r[14] = self.insn_addr.wrapping_add(4);
                    self.record_call(self.insn_addr, target);
                }
                self.branch(target);
                3
            }
            6 => {
                // LDC / STC: no coprocessor we model supports them.
                self.undefined();
                3
            }
            _ => {
                if insn & (1 << 24) != 0 {
                    // SWI
                    self.enter_exception(Exception::Swi, self.insn_addr.wrapping_add(4));
                    3
                } else if insn & (1 << 4) != 0 {
                    self.exec_coprocessor_reg(insn)
                } else {
                    // CDP
                    self.undefined();
                    3
                }
            }
        }
    }

    /// The cond == 0b1111 space, which on ARMv5 holds unconditional
    /// instructions rather than a "never" condition.
    fn execute_unconditional(&mut self, insn: u32) -> u32 {
        if insn & 0xFE00_0000 == 0xFA00_0000 {
            // BLX (immediate): the H bit contributes a halfword to the target.
            let offset = ((insn << 8) as i32 >> 6) as u32;
            let h = (insn >> 24) & 1;
            self.r[14] = self.insn_addr.wrapping_add(4);
            let target = self.r[15].wrapping_add(offset).wrapping_add(h << 1);
            self.record_call(self.insn_addr, target);
            self.cpsr |= T_BIT;
            self.r[15] = target & !1;
            self.branched = true;
            return 3;
        }
        // PLD and the cache/barrier hints are architecturally no-ops for us.
        if insn & 0x0D70_F000 == 0x0550_F000 {
            return 1;
        }
        self.undefined();
        3
    }

    pub(crate) fn undefined(&mut self) {
        if self.undefined_log.len() < 64 {
            let entry = (self.insn_addr, self.current_insn);
            if !self.undefined_log.contains(&entry) {
                self.undefined_log.push(entry);
            }
        }
        let ret = if self.thumb() {
            self.insn_addr.wrapping_add(2)
        } else {
            self.insn_addr.wrapping_add(4)
        };
        self.enter_exception(Exception::Undefined, ret);
    }

    /// Turn a data abort into an exception. Returns the cycle cost.
    #[inline]
    fn abort(&mut self, fault: Fault) -> u32 {
        self.data_abort(fault);
        3
    }

    // ---- data processing -------------------------------------------------

    fn exec_data_processing(&mut self, insn: u32) -> u32 {
        let opcode = (insn >> 21) & 0xF;
        let set_flags = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xF) as usize;
        let rd = ((insn >> 12) & 0xF) as usize;
        let carry_in = self.c();

        let (op2, shifter_carry, pc_extra) = if insn & (1 << 25) != 0 {
            // Rotated 8-bit immediate.
            let imm = insn & 0xFF;
            let rot = ((insn >> 8) & 0xF) * 2;
            if rot == 0 {
                (imm, carry_in, 0)
            } else {
                let v = imm.rotate_right(rot);
                (v, v & 0x8000_0000 != 0, 0)
            }
        } else {
            let kind = (insn >> 5) & 3;
            let rm = (insn & 0xF) as usize;
            if insn & (1 << 4) != 0 {
                // Shift amount from the bottom byte of Rs. PC reads as +12.
                let rs = ((insn >> 8) & 0xF) as usize;
                let amount = self.op_reg(rs, 4) & 0xFF;
                let val = self.op_reg(rm, 4);
                let s = shift_register(kind, val, amount, carry_in);
                (s.value, s.carry, 4)
            } else {
                let amount = (insn >> 7) & 0x1F;
                let s = shift_immediate(kind, self.r[rm], amount, carry_in);
                (s.value, s.carry, 0)
            }
        };

        let a = self.op_reg(rn, pc_extra);
        let mut logical = true;
        let (result, carry, overflow) = match opcode {
            0x0 | 0x8 => (a & op2, shifter_carry, false),          // AND, TST
            0x1 | 0x9 => (a ^ op2, shifter_carry, false),          // EOR, TEQ
            0x2 | 0xA => {
                logical = false;
                sub_with_borrow(a, op2, true)
            } // SUB, CMP
            0x3 => {
                logical = false;
                sub_with_borrow(op2, a, true)
            } // RSB
            0x4 | 0xB => {
                logical = false;
                add_with_carry(a, op2, false)
            } // ADD, CMN
            0x5 => {
                logical = false;
                add_with_carry(a, op2, carry_in)
            } // ADC
            0x6 => {
                logical = false;
                sub_with_borrow(a, op2, carry_in)
            } // SBC
            0x7 => {
                logical = false;
                sub_with_borrow(op2, a, carry_in)
            } // RSC
            0xC => (a | op2, shifter_carry, false),                // ORR
            0xD => (op2, shifter_carry, false),                    // MOV
            0xE => (a & !op2, shifter_carry, false),               // BIC
            _ => (!op2, shifter_carry, false),                     // MVN
        };

        // TST, TEQ, CMP and CMN discard the result.
        let writes_result = !matches!(opcode, 0x8..=0xB);

        if writes_result && rd == 15 {
            if set_flags {
                // Return from exception: restore CPSR, then branch. The T bit
                // in the restored CPSR decides the instruction set.
                let spsr = self.spsr;
                self.write_cpsr(spsr);
                let mask = if self.thumb() { !1 } else { !3 };
                self.r[15] = result & mask;
                self.branched = true;
            } else {
                self.branch(result);
            }
            return 3;
        }

        if writes_result {
            self.set_reg(rd, result);
        }
        if set_flags {
            self.set_nz(result);
            self.set_flag(C_BIT, carry);
            if !logical {
                self.set_flag(V_BIT, overflow);
            }
        }
        1
    }

    fn exec_msr(&mut self, insn: u32) -> u32 {
        let to_spsr = insn & (1 << 22) != 0;
        let fields = (insn >> 16) & 0xF;
        let val = if insn & (1 << 25) != 0 {
            let imm = insn & 0xFF;
            imm.rotate_right(((insn >> 8) & 0xF) * 2)
        } else {
            self.r[(insn & 0xF) as usize]
        };

        let mut mask = 0u32;
        if fields & 1 != 0 {
            mask |= 0x0000_00FF;
        }
        if fields & 2 != 0 {
            mask |= 0x0000_FF00;
        }
        if fields & 4 != 0 {
            mask |= 0x00FF_0000;
        }
        if fields & 8 != 0 {
            mask |= 0xFF00_0000;
        }

        if to_spsr {
            self.spsr = (self.spsr & !mask) | (val & mask);
        } else {
            // User mode may only touch the condition flags.
            if !self.privileged() {
                mask &= 0xF800_0000;
            }
            let new = (self.cpsr & !mask) | (val & mask);
            self.write_cpsr(new);
        }
        1
    }

    // ---- multiply --------------------------------------------------------

    fn exec_multiply(&mut self, insn: u32) -> u32 {
        let rd = ((insn >> 16) & 0xF) as usize;
        let rn = ((insn >> 12) & 0xF) as usize;
        let rs = ((insn >> 8) & 0xF) as usize;
        let rm = (insn & 0xF) as usize;
        let mut result = self.r[rm].wrapping_mul(self.r[rs]);
        if insn & (1 << 21) != 0 {
            result = result.wrapping_add(self.r[rn]);
        }
        self.set_reg(rd, result);
        if insn & (1 << 20) != 0 {
            self.set_nz(result);
        }
        4
    }

    fn exec_multiply_long(&mut self, insn: u32) -> u32 {
        let rd_hi = ((insn >> 16) & 0xF) as usize;
        let rd_lo = ((insn >> 12) & 0xF) as usize;
        let rs = self.r[((insn >> 8) & 0xF) as usize];
        let rm = self.r[(insn & 0xF) as usize];
        let signed = insn & (1 << 22) != 0;
        let accumulate = insn & (1 << 21) != 0;

        let mut result = if signed {
            ((rm as i32 as i64).wrapping_mul(rs as i32 as i64)) as u64
        } else {
            (rm as u64).wrapping_mul(rs as u64)
        };
        if accumulate {
            let acc = ((self.r[rd_hi] as u64) << 32) | self.r[rd_lo] as u64;
            result = result.wrapping_add(acc);
        }
        self.set_reg(rd_lo, result as u32);
        self.set_reg(rd_hi, (result >> 32) as u32);
        if insn & (1 << 20) != 0 {
            self.set_flag(N_BIT, result & 0x8000_0000_0000_0000 != 0);
            self.set_flag(Z_BIT, result == 0);
        }
        5
    }

    /// QADD, QSUB, QDADD, QDSUB.
    fn exec_saturating(&mut self, insn: u32) -> u32 {
        let op = (insn >> 21) & 3;
        let rn = self.r[((insn >> 16) & 0xF) as usize] as i32 as i64;
        let rd = ((insn >> 12) & 0xF) as usize;
        let rm = self.r[(insn & 0xF) as usize] as i32 as i64;

        let (value, saturated) = match op {
            0 => saturate_i32(rm + rn),
            1 => saturate_i32(rm - rn),
            2 => {
                let (doubled, sat1) = saturate_i32(rn * 2);
                let (v, sat2) = saturate_i32(rm + doubled as i32 as i64);
                (v, sat1 || sat2)
            }
            _ => {
                let (doubled, sat1) = saturate_i32(rn * 2);
                let (v, sat2) = saturate_i32(rm - doubled as i32 as i64);
                (v, sat1 || sat2)
            }
        };
        self.set_reg(rd, value);
        if saturated {
            self.cpsr |= Q_BIT;
        }
        1
    }

    /// The v5TE signed multiply block: SMLAxy, SMLAWy, SMULWy, SMLALxy,
    /// SMULxy. KeySoft's speech synthesis leans on these.
    fn exec_signed_multiply(&mut self, insn: u32) -> u32 {
        let op = (insn >> 21) & 3;
        let rd = ((insn >> 16) & 0xF) as usize;
        let rn = ((insn >> 12) & 0xF) as usize;
        let rs_i = ((insn >> 8) & 0xF) as usize;
        let rm_i = (insn & 0xF) as usize;
        let x = insn & (1 << 5) != 0; // selects the half of Rm
        let y = insn & (1 << 6) != 0; // selects the half of Rs

        let rm = self.r[rm_i];
        let rs = self.r[rs_i];
        let half = |v: u32, high: bool| -> i32 {
            if high {
                (v >> 16) as i16 as i32
            } else {
                v as i16 as i32
            }
        };

        match op {
            // SMLAxy
            0 => {
                let product = (half(rm, x) as i64) * (half(rs, y) as i64);
                let acc = self.r[rn] as i32 as i64;
                let sum = product + acc;
                let (v, overflow) = saturate_i32(sum);
                // SMLA sets Q on overflow but does not saturate the result.
                self.set_reg(rd, sum as u32);
                let _ = v;
                if overflow {
                    self.cpsr |= Q_BIT;
                }
            }
            // SMLAWy / SMULWy: 32x16 keeping the top 32 bits of a 48-bit
            // product.
            1 => {
                let product = (rm as i32 as i64) * (half(rs, y) as i64);
                let top = (product >> 16) as i32;
                if x {
                    // SMULWy: no accumulate.
                    self.set_reg(rd, top as u32);
                } else {
                    let sum = (top as i64) + (self.r[rn] as i32 as i64);
                    let (_, overflow) = saturate_i32(sum);
                    self.set_reg(rd, sum as u32);
                    if overflow {
                        self.cpsr |= Q_BIT;
                    }
                }
            }
            // SMLALxy
            2 => {
                let product = (half(rm, x) as i64) * (half(rs, y) as i64);
                let acc = ((self.r[rd] as u64) << 32 | self.r[rn] as u64) as i64;
                let sum = acc.wrapping_add(product) as u64;
                self.set_reg(rn, sum as u32);
                self.set_reg(rd, (sum >> 32) as u32);
            }
            // SMULxy
            _ => {
                let product = half(rm, x).wrapping_mul(half(rs, y));
                self.set_reg(rd, product as u32);
            }
        }
        3
    }

    // ---- memory ----------------------------------------------------------

    fn exec_swap<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let rn = self.r[((insn >> 16) & 0xF) as usize];
        let rd = ((insn >> 12) & 0xF) as usize;
        let rm = self.r[(insn & 0xF) as usize];
        let byte = insn & (1 << 22) != 0;

        if byte {
            match self.read_u8(bus, rn) {
                Ok(old) => {
                    if let Err(f) = self.write_u8(bus, rn, rm as u8) {
                        return self.abort(f);
                    }
                    self.set_reg(rd, old as u32);
                }
                Err(f) => return self.abort(f),
            }
        } else {
            match self.read_u32(bus, rn) {
                Ok(old) => {
                    if let Err(f) = self.write_u32(bus, rn, rm) {
                        return self.abort(f);
                    }
                    self.set_reg(rd, old);
                }
                Err(f) => return self.abort(f),
            }
        }
        4
    }

    /// Halfword, signed byte, signed halfword and doubleword transfers.
    fn exec_extra_transfer<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let pre = insn & (1 << 24) != 0;
        let up = insn & (1 << 23) != 0;
        let imm = insn & (1 << 22) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xF) as usize;
        let rd = ((insn >> 12) & 0xF) as usize;
        let sh = (insn >> 5) & 3;

        let offset = if imm {
            ((insn >> 4) & 0xF0) | (insn & 0xF)
        } else {
            self.r[(insn & 0xF) as usize]
        };

        let base = self.r[rn];
        let offset_addr =
            if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if pre { offset_addr } else { base };

        let result = (|| -> Result<(), Fault> {
            match (sh, load) {
                (1, true) => {
                    let v = self.read_u16(bus, addr)?;
                    self.set_reg(rd, v as u32);
                }
                (1, false) => {
                    let v = self.r[rd] as u16;
                    self.write_u16(bus, addr, v)?;
                }
                (2, true) => {
                    let v = self.read_u8(bus, addr)?;
                    self.set_reg(rd, v as i8 as i32 as u32);
                }
                (3, true) => {
                    let v = self.read_u16(bus, addr)?;
                    self.set_reg(rd, v as i16 as i32 as u32);
                }
                // LDRD / STRD operate on Rd and Rd+1.
                (2, false) => {
                    let lo = self.read_u32(bus, addr)?;
                    let hi = self.read_u32(bus, addr.wrapping_add(4))?;
                    self.set_reg(rd, lo);
                    self.set_reg(rd + 1, hi);
                }
                _ => {
                    let lo = self.r[rd];
                    let hi = self.r[rd + 1];
                    self.write_u32(bus, addr, lo)?;
                    self.write_u32(bus, addr.wrapping_add(4), hi)?;
                }
            }
            Ok(())
        })();

        if let Err(f) = result {
            return self.abort(f);
        }
        if (writeback || !pre) && rn != 15 {
            self.r[rn] = offset_addr;
        }
        if load {
            3
        } else {
            2
        }
    }

    fn exec_single_transfer<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let pre = insn & (1 << 24) != 0;
        let up = insn & (1 << 23) != 0;
        let byte = insn & (1 << 22) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xF) as usize;
        let rd = ((insn >> 12) & 0xF) as usize;

        let offset = if insn & (1 << 25) != 0 {
            // Register offset with an immediate shift. Bit 4 set here is a
            // media instruction on later architectures; undefined on v5.
            if insn & (1 << 4) != 0 {
                self.undefined();
                return 3;
            }
            let kind = (insn >> 5) & 3;
            let amount = (insn >> 7) & 0x1F;
            shift_immediate(kind, self.r[(insn & 0xF) as usize], amount, self.c()).value
        } else {
            insn & 0xFFF
        };

        let base = self.op_reg(rn, 0);
        let offset_addr =
            if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if pre { offset_addr } else { base };

        // A post-indexed transfer with W set is LDRT/STRT: it performs the
        // access with User-mode privilege even from a privileged mode.
        let translated_as_user = !pre && writeback;
        if translated_as_user {
            self.force_user = true;
        }

        let result = (|| -> Result<(), Fault> {
            match (load, byte) {
                (true, true) => {
                    let v = self.read_u8(bus, addr)?;
                    self.pending_load = Some((rd, v as u32));
                }
                (true, false) => {
                    let v = self.read_u32(bus, addr)?;
                    self.pending_load = Some((rd, v));
                }
                (false, true) => {
                    let v = self.op_reg(rd, 4) as u8;
                    self.write_u8(bus, addr, v)?;
                }
                (false, false) => {
                    let v = self.op_reg(rd, 4);
                    self.write_u32(bus, addr, v)?;
                }
            }
            Ok(())
        })();
        self.force_user = false;

        if let Err(f) = result {
            self.pending_load = None;
            return self.abort(f);
        }

        // Base writeback happens before the loaded value lands, so a load
        // into the base register wins.
        if (writeback || !pre) && rn != rd {
            self.r[rn] = offset_addr;
        }
        if let Some((reg, val)) = self.pending_load.take() {
            if reg == 15 {
                // LDR into PC interworks on ARMv5.
                self.branch_exchange(val);
            } else {
                self.set_reg(reg, val);
            }
        }
        if load {
            3
        } else {
            2
        }
    }

    fn exec_block_transfer<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let pre = insn & (1 << 24) != 0;
        let up = insn & (1 << 23) != 0;
        let s_bit = insn & (1 << 22) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xF) as usize;
        let list = insn & 0xFFFF;
        let count = list.count_ones();
        let base = self.r[rn];

        // Registers always move lowest-numbered to lowest address, whatever
        // the addressing mode, so normalise to an ascending walk.
        let start = if up {
            if pre {
                base.wrapping_add(4)
            } else {
                base
            }
        } else if pre {
            base.wrapping_sub(4 * count)
        } else {
            base.wrapping_sub(4 * count).wrapping_add(4)
        };
        let final_base =
            if up { base.wrapping_add(4 * count) } else { base.wrapping_sub(4 * count) };

        // With S set and PC absent from the list, the transfer uses the User
        // bank rather than the current mode's registers.
        let user_bank = s_bit && (!load || list & 0x8000 == 0);
        let restore_cpsr = s_bit && load && list & 0x8000 != 0;

        let mut addr = start;
        let mut loaded_pc = None;

        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            if load {
                match self.read_u32_aligned(bus, addr) {
                    Ok(v) => {
                        if i == 15 {
                            loaded_pc = Some(v);
                        } else if user_bank {
                            self.set_user_reg(i, v);
                        } else {
                            self.r[i] = v;
                        }
                    }
                    Err(f) => return self.abort(f),
                }
            } else {
                let v = if i == 15 {
                    self.r[15].wrapping_add(4)
                } else if user_bank {
                    self.user_reg(i)
                } else {
                    self.r[i]
                };
                if let Err(f) = self.write_u32(bus, addr, v) {
                    return self.abort(f);
                }
            }
            addr = addr.wrapping_add(4);
        }

        if writeback && !(load && list & (1 << rn) != 0) {
            self.r[rn] = final_base;
        }

        if let Some(pc) = loaded_pc {
            if restore_cpsr {
                let spsr = self.spsr;
                self.write_cpsr(spsr);
                let mask = if self.thumb() { !1 } else { !3 };
                self.r[15] = pc & mask;
                self.branched = true;
            } else {
                self.branch_exchange(pc);
            }
        }

        count + 2
    }

    // ---- coprocessor -----------------------------------------------------

    fn exec_coprocessor_reg(&mut self, insn: u32) -> u32 {
        let cp = (insn >> 8) & 0xF;
        let op1 = (insn >> 21) & 7;
        let crn = (insn >> 16) & 0xF;
        let rd = ((insn >> 12) & 0xF) as usize;
        let crm = insn & 0xF;
        let op2 = (insn >> 5) & 7;
        let load = insn & (1 << 20) != 0;

        match cp {
            15 => {
                if load {
                    let val = self.cp15.read(crn, crm, op1, op2);
                    if rd == 15 {
                        // MRC to PC updates the flags, not the program counter.
                        self.cpsr = (self.cpsr & 0x0FFF_FFFF) | (val & 0xF000_0000);
                    } else {
                        self.set_reg(rd, val);
                    }
                } else {
                    let val = self.r[rd];
                    self.cp15.write(crn, crm, op1, op2, val);
                }
                2
            }
            // CP14 c7 is the XScale power mode register. Writing a
            // non-zero mode idles or sleeps the core until an interrupt
            // wakes it. Windows CE's idle path depends on the core actually
            // stopping: when it does not, execution runs off the end of the
            // sleep routine into a fallback that toggles a GPIO forever.
            14 if crn == 7 && !load => {
                let mode = self.r[rd] & 7;
                self.pwrmode = mode as u8;
                if mode != 0 {
                    self.halted = true;
                }
                // Idle stops the core until an interrupt. Sleep and deeper
                // are a real power-down: the PXA drops most of the chip and
                // resumes through the reset vector on a configured wake
                // source, so an ordinary interrupt must not restart it.
                self.suspended = mode >= PWRMODE_SLEEP as u32;
                2
            }
            14 if crn == 7 && load => {
                self.set_reg(rd, self.pwrmode as u32);
                2
            }
            // The rest of CP14 is the XScale debug unit, and CP0/CP1 are
            // iWMMXt. The OAL probes them; zeroes keep it on its
            // non-accelerated path instead of faulting.
            0 | 1 | 14 => {
                if load {
                    self.set_reg(rd, 0);
                }
                2
            }
            _ => {
                self.undefined();
                3
            }
        }
    }
}
