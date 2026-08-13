//! The physical memory bus seen by the CPU.
//!
//! Everything below the CPU (RAM, flash, SoC peripherals, board glue) is
//! reached through this trait, always by *physical* address. Virtual-to-
//! physical translation is the CPU's job and happens before any call here.

/// A device or memory region reachable at a physical address.
///
/// Widths are separate methods rather than a size parameter because most
/// peripherals care about access width, and several PXA registers behave
/// differently for byte and word accesses.
pub trait Bus {
    fn read8(&mut self, pa: u32) -> u8;
    fn read16(&mut self, pa: u32) -> u16;
    fn read32(&mut self, pa: u32) -> u32;

    fn write8(&mut self, pa: u32, val: u8);
    fn write16(&mut self, pa: u32, val: u16);
    fn write32(&mut self, pa: u32, val: u32);

    /// Instruction fetch. Separate from `read32` because some devices answer
    /// a fetch differently from a data read: NOR flash in a command mode
    /// returns status or CFI data to a load, but code executing out of that
    /// same flash still needs the array. On hardware the distinction comes
    /// from the instruction cache; here it comes from this method.
    fn fetch32(&mut self, pa: u32) -> u32 {
        self.read32(pa)
    }

    fn fetch16(&mut self, pa: u32) -> u16 {
        self.read16(pa)
    }

    /// Advance peripheral time by `cycles` CPU cycles.
    ///
    /// Called by the CPU as it retires instructions so timers, the interrupt
    /// controller and DMA stay in step without a separate scheduler thread.
    fn tick(&mut self, cycles: u32) {
        let _ = cycles;
    }

    /// Level of the IRQ line into the core, sampled between instructions.
    fn irq_pending(&self) -> bool {
        false
    }

    /// Level of the FIQ line into the core.
    fn fiq_pending(&self) -> bool {
        false
    }

    /// The bus's directly-addressable RAM, if it has any.
    ///
    /// This exists so that a TLB hit can produce an *index* rather than a
    /// physical address that then has to be dispatched all over again. The
    /// dispatch is a chain of range compares with guards on it, and it runs
    /// on every load and store; the index is a slice access. It is also what
    /// compiled code needs, because generated code cannot walk a match.
    fn ram(&self) -> &[u8] {
        &[]
    }

    fn ram_mut(&mut self) -> &mut [u8] {
        &mut []
    }

    /// Where `pa` sits in `ram()`, if the whole `len` bytes from there are
    /// plain RAM.
    ///
    /// Asked about a whole TLB granule when an entry is filled, never per
    /// access, and answering `Some` is a promise that every byte of that
    /// granule is in bounds -- which is what lets the fast path skip a
    /// second range check.
    fn ram_offset(&self, pa: u32, len: u32) -> Option<u32> {
        let _ = (pa, len);
        None
    }
}

/// A flat byte-addressed region, used for RAM and for tests.
pub struct Ram {
    pub base: u32,
    pub data: Vec<u8>,
}

impl Ram {
    pub fn new(base: u32, size: usize) -> Self {
        Ram { base, data: vec![0; size] }
    }

    #[inline]
    pub fn contains(&self, pa: u32) -> bool {
        pa >= self.base && ((pa - self.base) as usize) < self.data.len()
    }

    #[inline]
    fn off(&self, pa: u32) -> usize {
        (pa - self.base) as usize
    }
}

impl Bus for Ram {
    fn ram(&self) -> &[u8] {
        &self.data
    }

    fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn ram_offset(&self, pa: u32, len: u32) -> Option<u32> {
        if !self.contains(pa) {
            return None;
        }
        let off = self.off(pa);
        (off + len as usize <= self.data.len()).then_some(off as u32)
    }

    #[inline]
    fn read8(&mut self, pa: u32) -> u8 {
        let o = self.off(pa);
        self.data.get(o).copied().unwrap_or(0)
    }
    #[inline]
    fn read16(&mut self, pa: u32) -> u16 {
        let o = self.off(pa & !1);
        if o + 2 > self.data.len() {
            return 0;
        }
        u16::from_le_bytes([self.data[o], self.data[o + 1]])
    }
    #[inline]
    fn read32(&mut self, pa: u32) -> u32 {
        let o = self.off(pa & !3);
        if o + 4 > self.data.len() {
            return 0;
        }
        u32::from_le_bytes([self.data[o], self.data[o + 1], self.data[o + 2], self.data[o + 3]])
    }
    #[inline]
    fn write8(&mut self, pa: u32, val: u8) {
        let o = self.off(pa);
        if let Some(b) = self.data.get_mut(o) {
            *b = val;
        }
    }
    #[inline]
    fn write16(&mut self, pa: u32, val: u16) {
        let o = self.off(pa & !1);
        if o + 2 <= self.data.len() {
            self.data[o..o + 2].copy_from_slice(&val.to_le_bytes());
        }
    }
    #[inline]
    fn write32(&mut self, pa: u32, val: u32) {
        let o = self.off(pa & !3);
        if o + 4 <= self.data.len() {
            self.data[o..o + 4].copy_from_slice(&val.to_le_bytes());
        }
    }
}
