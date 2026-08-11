//! Barrel shifter and flag arithmetic.

#[derive(Copy, Clone)]
pub struct Shifted {
    pub value: u32,
    pub carry: bool,
}

pub const LSL: u32 = 0;
pub const LSR: u32 = 1;
pub const ASR: u32 = 2;
pub const ROR: u32 = 3;

/// Shift with an immediate amount, where an amount of zero has a special
/// meaning for every type except LSL.
pub fn shift_immediate(kind: u32, value: u32, amount: u32, carry_in: bool) -> Shifted {
    match kind {
        LSL => {
            if amount == 0 {
                Shifted { value, carry: carry_in }
            } else {
                Shifted { value: value << amount, carry: value & (1 << (32 - amount)) != 0 }
            }
        }
        LSR => {
            // LSR #0 encodes LSR #32.
            let amount = if amount == 0 { 32 } else { amount };
            if amount == 32 {
                Shifted { value: 0, carry: value & 0x8000_0000 != 0 }
            } else {
                Shifted { value: value >> amount, carry: value & (1 << (amount - 1)) != 0 }
            }
        }
        ASR => {
            // ASR #0 encodes ASR #32.
            let amount = if amount == 0 { 32 } else { amount };
            if amount >= 32 {
                let filled = if value & 0x8000_0000 != 0 { u32::MAX } else { 0 };
                Shifted { value: filled, carry: value & 0x8000_0000 != 0 }
            } else {
                Shifted {
                    value: ((value as i32) >> amount) as u32,
                    carry: value & (1 << (amount - 1)) != 0,
                }
            }
        }
        _ => {
            if amount == 0 {
                // ROR #0 encodes RRX: a 33-bit rotate through carry.
                let value_out = (value >> 1) | ((carry_in as u32) << 31);
                Shifted { value: value_out, carry: value & 1 != 0 }
            } else {
                Shifted {
                    value: value.rotate_right(amount),
                    carry: value & (1 << (amount - 1)) != 0,
                }
            }
        }
    }
}

/// Shift by an amount taken from the bottom byte of a register, where zero
/// means "leave alone" and amounts of 32 or more are well defined.
pub fn shift_register(kind: u32, value: u32, amount: u32, carry_in: bool) -> Shifted {
    let amount = amount & 0xFF;
    if amount == 0 {
        return Shifted { value, carry: carry_in };
    }
    match kind {
        LSL => {
            if amount < 32 {
                Shifted { value: value << amount, carry: value & (1 << (32 - amount)) != 0 }
            } else if amount == 32 {
                Shifted { value: 0, carry: value & 1 != 0 }
            } else {
                Shifted { value: 0, carry: false }
            }
        }
        LSR => {
            if amount < 32 {
                Shifted { value: value >> amount, carry: value & (1 << (amount - 1)) != 0 }
            } else if amount == 32 {
                Shifted { value: 0, carry: value & 0x8000_0000 != 0 }
            } else {
                Shifted { value: 0, carry: false }
            }
        }
        ASR => {
            if amount < 32 {
                Shifted {
                    value: ((value as i32) >> amount) as u32,
                    carry: value & (1 << (amount - 1)) != 0,
                }
            } else {
                let filled = if value & 0x8000_0000 != 0 { u32::MAX } else { 0 };
                Shifted { value: filled, carry: value & 0x8000_0000 != 0 }
            }
        }
        _ => {
            let rot = amount & 31;
            if rot == 0 {
                Shifted { value, carry: value & 0x8000_0000 != 0 }
            } else {
                Shifted { value: value.rotate_right(rot), carry: value & (1 << (rot - 1)) != 0 }
            }
        }
    }
}

/// `a + b + carry`, returning the result with carry-out and signed overflow.
#[inline]
pub fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let (r1, c1) = a.overflowing_add(b);
    let (result, c2) = r1.overflowing_add(carry_in as u32);
    let carry = c1 || c2;
    let overflow = ((a ^ result) & (b ^ result)) & 0x8000_0000 != 0;
    (result, carry, overflow)
}

/// `a - b`, following the ARM convention where carry means "no borrow".
#[inline]
pub fn sub_with_borrow(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    add_with_carry(a, !b, carry_in)
}

/// Saturate a 64-bit intermediate to a signed 32-bit result, reporting
/// whether saturation happened so QADD and friends can set the Q flag.
#[inline]
pub fn saturate_i32(v: i64) -> (u32, bool) {
    if v > i32::MAX as i64 {
        (i32::MAX as u32, true)
    } else if v < i32::MIN as i64 {
        (i32::MIN as u32, true)
    } else {
        (v as u32, false)
    }
}
