//! An ARMv5TE interpreter targeting the Intel XScale core in the PXA270.
//!
//! Scope is deliberately narrow: exactly what a Windows CE 4.2 kernel and its
//! drivers execute on a PXA270. That means ARM and Thumb state, the full
//! two-level MMU with FCSE, banked registers and the seven exception modes.
//! It does not include iWMMXt or the XScale debug unit beyond enough to stop
//! the OAL faulting when it probes for them.

pub mod alu;
pub mod arm;
pub mod bus;
pub mod cp15;
pub mod cpu;
pub mod mmu;
pub mod thumb;

pub use bus::{Bus, Ram};
pub use cpu::{Cpu, Exception};
pub use arm::PWRMODE_SLEEP;
pub use mmu::{Access, Fault};

#[cfg(test)]
mod tests;
