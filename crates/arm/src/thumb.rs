//! Thumb-state execution for ARMv5T.
//!
//! Most of Windows CE ships as ARMV4I, which is Thumb-interworking ARM code,
//! but coredll and several drivers contain Thumb, and the kernel's exception
//! paths interwork constantly.

use crate::alu::*;
use crate::bus::Bus;
use crate::cpu::*;
use crate::mmu::Fault;

impl Cpu {
    pub fn execute_thumb<B: Bus>(&mut self, bus: &mut B, insn: u16) -> u32 {
        let insn = insn as u32;
        match insn >> 12 {
            0b0000 | 0b0001 => self.thumb_shift_add_sub(insn),
            0b0010 | 0b0011 => self.thumb_immediate(insn),
            0b0100 => {
                if insn & 0x0800 != 0 {
                    // PC-relative load.
                    let rd = ((insn >> 8) & 7) as usize;
                    let addr = (self.r[15] & !3).wrapping_add((insn & 0xFF) * 4);
                    match self.read_u32(bus, addr) {
                        Ok(v) => self.set_reg(rd, v),
                        Err(f) => return self.thumb_abort(f),
                    }
                    3
                } else if insn & 0x0400 != 0 {
                    self.thumb_hi_register(insn)
                } else {
                    self.thumb_alu(insn)
                }
            }
            0b0101 => self.thumb_transfer_register_offset(bus, insn),
            0b0110 | 0b0111 => self.thumb_transfer_immediate(bus, insn),
            0b1000 => self.thumb_transfer_halfword(bus, insn),
            0b1001 => self.thumb_transfer_sp_relative(bus, insn),
            0b1010 => {
                // ADD Rd, PC/SP, #imm
                let rd = ((insn >> 8) & 7) as usize;
                let imm = (insn & 0xFF) * 4;
                let base = if insn & 0x0800 != 0 { self.r[13] } else { self.r[15] & !3 };
                self.set_reg(rd, base.wrapping_add(imm));
                1
            }
            0b1011 => self.thumb_misc(bus, insn),
            0b1100 => self.thumb_block_transfer(bus, insn),
            0b1101 => {
                let cond = (insn >> 8) & 0xF;
                if cond == 0xF {
                    self.enter_exception(Exception::Swi, self.insn_addr.wrapping_add(2));
                    return 3;
                }
                if cond == 0xE {
                    self.undefined();
                    return 3;
                }
                if self.cond_passes(cond) {
                    let offset = ((insn & 0xFF) as u8 as i8 as i32 * 2) as u32;
                    let target = self.r[15].wrapping_add(offset);
                    self.branch(target);
                    3
                } else {
                    1
                }
            }
            0b1110 => {
                if insn & 0x0800 == 0 {
                    // Unconditional branch, 11-bit signed offset.
                    let offset = (((insn & 0x7FF) << 21) as i32 >> 20) as u32;
                    let target = self.r[15].wrapping_add(offset);
                    self.branch(target);
                    3
                } else {
                    // Second half of a BLX pair.
                    self.thumb_branch_link(insn, true)
                }
            }
            _ => self.thumb_branch_link(insn, false),
        }
    }

    #[inline]
    fn thumb_abort(&mut self, fault: Fault) -> u32 {
        self.data_abort(fault);
        3
    }

    fn thumb_shift_add_sub(&mut self, insn: u32) -> u32 {
        let rd = (insn & 7) as usize;
        let rs = ((insn >> 3) & 7) as usize;
        let op = (insn >> 11) & 3;

        if op == 3 {
            // ADD/SUB with a register or 3-bit immediate operand.
            let sub = insn & 0x0200 != 0;
            let operand = if insn & 0x0400 != 0 {
                (insn >> 6) & 7
            } else {
                self.r[((insn >> 6) & 7) as usize]
            };
            let (result, carry, overflow) = if sub {
                sub_with_borrow(self.r[rs], operand, true)
            } else {
                add_with_carry(self.r[rs], operand, false)
            };
            self.set_reg(rd, result);
            self.set_nz(result);
            self.set_flag(C_BIT, carry);
            self.set_flag(V_BIT, overflow);
        } else {
            let amount = (insn >> 6) & 0x1F;
            let s = shift_immediate(op, self.r[rs], amount, self.c());
            self.set_reg(rd, s.value);
            self.set_nz(s.value);
            self.set_flag(C_BIT, s.carry);
        }
        1
    }

    fn thumb_immediate(&mut self, insn: u32) -> u32 {
        let rd = ((insn >> 8) & 7) as usize;
        let imm = insn & 0xFF;
        match (insn >> 11) & 3 {
            0 => {
                // MOV
                self.set_reg(rd, imm);
                self.set_nz(imm);
            }
            1 => {
                // CMP
                let (result, carry, overflow) = sub_with_borrow(self.r[rd], imm, true);
                self.set_nz(result);
                self.set_flag(C_BIT, carry);
                self.set_flag(V_BIT, overflow);
            }
            2 => {
                let (result, carry, overflow) = add_with_carry(self.r[rd], imm, false);
                self.set_reg(rd, result);
                self.set_nz(result);
                self.set_flag(C_BIT, carry);
                self.set_flag(V_BIT, overflow);
            }
            _ => {
                let (result, carry, overflow) = sub_with_borrow(self.r[rd], imm, true);
                self.set_reg(rd, result);
                self.set_nz(result);
                self.set_flag(C_BIT, carry);
                self.set_flag(V_BIT, overflow);
            }
        }
        1
    }

    fn thumb_alu(&mut self, insn: u32) -> u32 {
        let rd = (insn & 7) as usize;
        let rs = ((insn >> 3) & 7) as usize;
        let op = (insn >> 6) & 0xF;
        let a = self.r[rd];
        let b = self.r[rs];
        let carry_in = self.c();

        let mut cycles = 1;
        match op {
            0x0 => {
                let r = a & b;
                self.set_reg(rd, r);
                self.set_nz(r);
            }
            0x1 => {
                let r = a ^ b;
                self.set_reg(rd, r);
                self.set_nz(r);
            }
            0x2 | 0x3 | 0x4 | 0x7 => {
                let kind = match op {
                    0x2 => LSL,
                    0x3 => LSR,
                    0x4 => ASR,
                    _ => ROR,
                };
                let s = shift_register(kind, a, b, carry_in);
                self.set_reg(rd, s.value);
                self.set_nz(s.value);
                self.set_flag(C_BIT, s.carry);
                cycles = 2;
            }
            0x5 => {
                let (r, c, v) = add_with_carry(a, b, carry_in);
                self.set_reg(rd, r);
                self.set_nz(r);
                self.set_flag(C_BIT, c);
                self.set_flag(V_BIT, v);
            }
            0x6 => {
                let (r, c, v) = sub_with_borrow(a, b, carry_in);
                self.set_reg(rd, r);
                self.set_nz(r);
                self.set_flag(C_BIT, c);
                self.set_flag(V_BIT, v);
            }
            0x8 => {
                let r = a & b;
                self.set_nz(r);
            }
            0x9 => {
                // NEG
                let (r, c, v) = sub_with_borrow(0, b, true);
                self.set_reg(rd, r);
                self.set_nz(r);
                self.set_flag(C_BIT, c);
                self.set_flag(V_BIT, v);
            }
            0xA => {
                let (r, c, v) = sub_with_borrow(a, b, true);
                self.set_nz(r);
                self.set_flag(C_BIT, c);
                self.set_flag(V_BIT, v);
            }
            0xB => {
                let (r, c, v) = add_with_carry(a, b, false);
                self.set_nz(r);
                self.set_flag(C_BIT, c);
                self.set_flag(V_BIT, v);
            }
            0xC => {
                let r = a | b;
                self.set_reg(rd, r);
                self.set_nz(r);
            }
            0xD => {
                let r = a.wrapping_mul(b);
                self.set_reg(rd, r);
                self.set_nz(r);
                cycles = 4;
            }
            0xE => {
                let r = a & !b;
                self.set_reg(rd, r);
                self.set_nz(r);
            }
            _ => {
                let r = !b;
                self.set_reg(rd, r);
                self.set_nz(r);
            }
        }
        cycles
    }

    fn thumb_hi_register(&mut self, insn: u32) -> u32 {
        let op = (insn >> 8) & 3;
        let rd = ((insn & 7) | ((insn >> 4) & 8)) as usize;
        let rs = ((insn >> 3) & 0xF) as usize;

        match op {
            0 => {
                let v = self.r[rd].wrapping_add(self.r[rs]);
                if rd == 15 {
                    self.branch(v);
                    3
                } else {
                    self.set_reg(rd, v);
                    1
                }
            }
            1 => {
                let (r, c, v) = sub_with_borrow(self.r[rd], self.r[rs], true);
                self.set_nz(r);
                self.set_flag(C_BIT, c);
                self.set_flag(V_BIT, v);
                1
            }
            2 => {
                let v = self.r[rs];
                if rd == 15 {
                    self.branch(v);
                    3
                } else {
                    self.set_reg(rd, v);
                    1
                }
            }
            _ => {
                // BX / BLX (register). Bit 7 selects BLX.
                let target = self.r[rs];
                if insn & 0x0080 != 0 {
                    self.r[14] = self.insn_addr.wrapping_add(2) | 1;
                    self.record_call(self.insn_addr, target);
                }
                self.branch_exchange(target);
                3
            }
        }
    }

    fn thumb_transfer_register_offset<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let rd = (insn & 7) as usize;
        let rb = ((insn >> 3) & 7) as usize;
        let ro = ((insn >> 6) & 7) as usize;
        let addr = self.r[rb].wrapping_add(self.r[ro]);

        // Bit 9 separates the sign-extending group from the plain one. In
        // both, bits 11:10 pick the operation.
        let result = if insn & 0x0200 != 0 {
            match (insn >> 10) & 3 {
                0 => {
                    let v = self.r[rd] as u16;
                    self.write_u16(bus, addr, v)
                }
                1 => match self.read_u8(bus, addr) {
                    Ok(v) => {
                        self.set_reg(rd, v as i8 as i32 as u32);
                        Ok(())
                    }
                    Err(f) => Err(f),
                },
                2 => match self.read_u16(bus, addr) {
                    Ok(v) => {
                        self.set_reg(rd, v as u32);
                        Ok(())
                    }
                    Err(f) => Err(f),
                },
                _ => match self.read_u16(bus, addr) {
                    Ok(v) => {
                        self.set_reg(rd, v as i16 as i32 as u32);
                        Ok(())
                    }
                    Err(f) => Err(f),
                },
            }
        } else {
            match (insn >> 10) & 3 {
                0 => {
                    let v = self.r[rd];
                    self.write_u32(bus, addr, v)
                }
                1 => {
                    let v = self.r[rd] as u8;
                    self.write_u8(bus, addr, v)
                }
                2 => match self.read_u32(bus, addr) {
                    Ok(v) => {
                        self.set_reg(rd, v);
                        Ok(())
                    }
                    Err(f) => Err(f),
                },
                _ => match self.read_u8(bus, addr) {
                    Ok(v) => {
                        self.set_reg(rd, v as u32);
                        Ok(())
                    }
                    Err(f) => Err(f),
                },
            }
        };

        match result {
            Ok(()) => 3,
            Err(f) => self.thumb_abort(f),
        }
    }

    fn thumb_transfer_immediate<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let rd = (insn & 7) as usize;
        let rb = ((insn >> 3) & 7) as usize;
        let offset5 = (insn >> 6) & 0x1F;
        let byte = insn & 0x1000 != 0;
        let load = insn & 0x0800 != 0;

        let addr = if byte {
            self.r[rb].wrapping_add(offset5)
        } else {
            self.r[rb].wrapping_add(offset5 * 4)
        };

        let result = match (load, byte) {
            (true, true) => match self.read_u8(bus, addr) {
                Ok(v) => {
                    self.set_reg(rd, v as u32);
                    Ok(())
                }
                Err(f) => Err(f),
            },
            (true, false) => match self.read_u32(bus, addr) {
                Ok(v) => {
                    self.set_reg(rd, v);
                    Ok(())
                }
                Err(f) => Err(f),
            },
            (false, true) => {
                let v = self.r[rd] as u8;
                self.write_u8(bus, addr, v)
            }
            (false, false) => {
                let v = self.r[rd];
                self.write_u32(bus, addr, v)
            }
        };
        match result {
            Ok(()) => 3,
            Err(f) => self.thumb_abort(f),
        }
    }

    fn thumb_transfer_halfword<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let rd = (insn & 7) as usize;
        let rb = ((insn >> 3) & 7) as usize;
        let addr = self.r[rb].wrapping_add(((insn >> 6) & 0x1F) * 2);
        let result = if insn & 0x0800 != 0 {
            match self.read_u16(bus, addr) {
                Ok(v) => {
                    self.set_reg(rd, v as u32);
                    Ok(())
                }
                Err(f) => Err(f),
            }
        } else {
            let v = self.r[rd] as u16;
            self.write_u16(bus, addr, v)
        };
        match result {
            Ok(()) => 3,
            Err(f) => self.thumb_abort(f),
        }
    }

    fn thumb_transfer_sp_relative<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let rd = ((insn >> 8) & 7) as usize;
        let addr = self.r[13].wrapping_add((insn & 0xFF) * 4);
        let result = if insn & 0x0800 != 0 {
            match self.read_u32(bus, addr) {
                Ok(v) => {
                    self.set_reg(rd, v);
                    Ok(())
                }
                Err(f) => Err(f),
            }
        } else {
            let v = self.r[rd];
            self.write_u32(bus, addr, v)
        };
        match result {
            Ok(()) => 3,
            Err(f) => self.thumb_abort(f),
        }
    }

    fn thumb_misc<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        if insn & 0x0F00 == 0x0000 {
            // ADD/SUB immediate to SP.
            let imm = (insn & 0x7F) * 4;
            self.r[13] = if insn & 0x0080 != 0 {
                self.r[13].wrapping_sub(imm)
            } else {
                self.r[13].wrapping_add(imm)
            };
            return 1;
        }
        if insn & 0x0F00 == 0x0E00 {
            // BKPT: no debugger attached, so treat it as a prefetch abort the
            // way the architecture specifies.
            self.enter_exception(Exception::PrefetchAbort, self.insn_addr.wrapping_add(4));
            return 3;
        }
        if insn & 0x0600 == 0x0400 {
            return self.thumb_push_pop(bus, insn);
        }
        self.undefined();
        3
    }

    fn thumb_push_pop<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let load = insn & 0x0800 != 0;
        let extra = insn & 0x0100 != 0; // LR on push, PC on pop
        let list = insn & 0xFF;
        let count = list.count_ones() + extra as u32;

        if load {
            let mut addr = self.r[13];
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    match self.read_u32_aligned(bus, addr) {
                        Ok(v) => self.r[i] = v,
                        Err(f) => return self.thumb_abort(f),
                    }
                    addr = addr.wrapping_add(4);
                }
            }
            let mut pc = None;
            if extra {
                match self.read_u32_aligned(bus, addr) {
                    Ok(v) => pc = Some(v),
                    Err(f) => return self.thumb_abort(f),
                }
                addr = addr.wrapping_add(4);
            }
            self.r[13] = addr;
            if let Some(pc) = pc {
                // POP {..., PC} interworks on ARMv5.
                self.branch_exchange(pc);
            }
        } else {
            let mut addr = self.r[13].wrapping_sub(4 * count);
            self.r[13] = addr;
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    let v = self.r[i];
                    if let Err(f) = self.write_u32(bus, addr, v) {
                        return self.thumb_abort(f);
                    }
                    addr = addr.wrapping_add(4);
                }
            }
            if extra {
                let v = self.r[14];
                if let Err(f) = self.write_u32(bus, addr, v) {
                    return self.thumb_abort(f);
                }
            }
        }
        count + 2
    }

    fn thumb_block_transfer<B: Bus>(&mut self, bus: &mut B, insn: u32) -> u32 {
        let rb = ((insn >> 8) & 7) as usize;
        let list = insn & 0xFF;
        let load = insn & 0x0800 != 0;
        let mut addr = self.r[rb];

        for i in 0..8 {
            if list & (1 << i) == 0 {
                continue;
            }
            if load {
                match self.read_u32_aligned(bus, addr) {
                    Ok(v) => self.r[i] = v,
                    Err(f) => return self.thumb_abort(f),
                }
            } else {
                let v = self.r[i];
                if let Err(f) = self.write_u32(bus, addr, v) {
                    return self.thumb_abort(f);
                }
            }
            addr = addr.wrapping_add(4);
        }

        // On a load that included the base register, the loaded value wins.
        if !(load && list & (1 << rb) != 0) {
            self.r[rb] = addr;
        }
        list.count_ones() + 2
    }

    /// The two halves of BL, and the ARMv5 BLX variant.
    fn thumb_branch_link(&mut self, insn: u32, exchange: bool) -> u32 {
        let offset = insn & 0x7FF;
        if !exchange && insn & 0x0800 == 0 {
            // First half: LR = PC + (signed offset << 12).
            let high = ((offset << 21) as i32 >> 9) as u32;
            self.r[14] = self.r[15].wrapping_add(high);
            return 1;
        }

        let target = self.r[14].wrapping_add(offset << 1);
        self.record_call(self.insn_addr, target);
        self.r[14] = self.insn_addr.wrapping_add(2) | 1;
        if exchange {
            // BLX: land in ARM state on a word boundary.
            self.cpsr &= !T_BIT;
            self.r[15] = target & !3;
            self.branched = true;
        } else {
            self.branch(target);
        }
        3
    }
}
