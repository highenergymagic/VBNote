use crate::bus::{Bus, Ram};
use crate::cpu::*;

/// Flat RAM at physical 0 with a settable IRQ line.
struct TestBus {
    ram: Ram,
    irq: bool,
}

impl TestBus {
    fn new() -> Self {
        TestBus { ram: Ram::new(0, 4 << 20), irq: false }
    }
    fn load(&mut self, addr: u32, words: &[u32]) {
        for (i, w) in words.iter().enumerate() {
            self.ram.write32(addr + 4 * i as u32, *w);
        }
    }
    fn load16(&mut self, addr: u32, halfwords: &[u16]) {
        for (i, w) in halfwords.iter().enumerate() {
            self.ram.write16(addr + 2 * i as u32, *w);
        }
    }
}

impl Bus for TestBus {
    fn read8(&mut self, pa: u32) -> u8 {
        self.ram.read8(pa)
    }
    fn read16(&mut self, pa: u32) -> u16 {
        self.ram.read16(pa)
    }
    fn read32(&mut self, pa: u32) -> u32 {
        self.ram.read32(pa)
    }
    fn write8(&mut self, pa: u32, v: u8) {
        self.ram.write8(pa, v)
    }
    fn write16(&mut self, pa: u32, v: u16) {
        self.ram.write16(pa, v)
    }
    fn write32(&mut self, pa: u32, v: u32) {
        self.ram.write32(pa, v)
    }
    fn irq_pending(&self) -> bool {
        self.irq
    }
}

fn run(words: &[u32], steps: usize) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.load(0, words);
    let mut cpu = Cpu::new();
    cpu.cpsr = MODE_SVC | I_BIT | F_BIT;
    for _ in 0..steps {
        cpu.step(&mut bus);
    }
    (cpu, bus)
}

#[test]
fn mov_immediate() {
    // mov r0, #0x2A
    let (cpu, _) = run(&[0xE3A0_002A], 1);
    assert_eq!(cpu.r[0], 0x2A);
    assert_eq!(cpu.r[15], 4);
}

#[test]
fn rotated_immediate() {
    // mov r0, #0xFF000000  (imm 0xFF ror 8)
    let (cpu, _) = run(&[0xE3A0_04FF], 1);
    assert_eq!(cpu.r[0], 0xFF00_0000);
}

#[test]
fn add_sets_carry_and_overflow() {
    let (cpu, _) = run(
        &[
            0xE3A0_0102, // mov r0, #0x80000000
            0xE1A0_1000, // mov r1, r0
            0xE091_2000, // adds r2, r1, r0
        ],
        3,
    );
    assert_eq!(cpu.r[2], 0);
    assert!(cpu.z(), "result is zero");
    assert!(cpu.c(), "carry out of the top");
    assert!(cpu.v(), "signed overflow");
}

#[test]
fn subs_borrow_convention() {
    // ARM sets C on "no borrow", so 5 - 3 must set C.
    let (cpu, _) = run(
        &[
            0xE3A0_0005, // mov r0, #5
            0xE250_1003, // subs r1, r0, #3
        ],
        2,
    );
    assert_eq!(cpu.r[1], 2);
    assert!(cpu.c());
    assert!(!cpu.n());
}

#[test]
fn barrel_shift_register_reads_pc_as_plus_twelve() {
    // mov r0, pc, lsl r1  with r1 = 0 reads PC as insn+12.
    let (cpu, _) = run(
        &[
            0xE3A0_1000, // mov r1, #0
            0xE1A0_011F, // mov r0, pc, lsl r1
        ],
        2,
    );
    assert_eq!(cpu.r[0], 4 + 12);
}

#[test]
fn store_then_load_word() {
    let (cpu, _) = run(
        &[
            0xE3A0_0F41, // mov r0, #0x104
            0xE3A0_10AB, // mov r1, #0xAB
            0xE580_1000, // str r1, [r0]
            0xE590_2000, // ldr r2, [r0]
        ],
        4,
    );
    assert_eq!(cpu.r[2], 0xAB);
}

#[test]
fn unaligned_word_load_rotates() {
    let mut bus = TestBus::new();
    bus.load(0x200, &[0x1122_3344]);
    bus.load(
        0,
        &[
            0xE3A0_0C02, // mov r0, #0x200
            0xE280_0001, // add r0, r0, #1
            0xE590_1000, // ldr r1, [r0]
        ],
    );
    let mut cpu = Cpu::new();
    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.r[1], 0x1122_3344u32.rotate_right(8));
}

#[test]
fn block_transfer_round_trip() {
    let (cpu, _) = run(
        &[
            0xE3A0_0F81, // mov r0, #0x204
            0xE3A0_1001, // mov r1, #1
            0xE3A0_2002, // mov r2, #2
            0xE3A0_3003, // mov r3, #3
            0xE880_000E, // stmia r0, {r1,r2,r3}
            0xE890_0070, // ldmia r0, {r4,r5,r6}
        ],
        6,
    );
    assert_eq!((cpu.r[4], cpu.r[5], cpu.r[6]), (1, 2, 3));
}

#[test]
fn push_pop_full_descending() {
    let (cpu, _) = run(
        &[
            0xE3A0_DC01, // mov sp, #0x100
            0xE3A0_0007, // mov r0, #7
            0xE92D_0001, // push {r0}
            0xE3A0_0000, // mov r0, #0
            0xE8BD_0002, // pop {r1}
        ],
        5,
    );
    assert_eq!(cpu.r[1], 7);
    assert_eq!(cpu.r[13], 0x100);
}

#[test]
fn branch_and_link_sets_lr() {
    let (cpu, _) = run(&[0xEB00_0002], 1); // bl +8 from pc
    assert_eq!(cpu.r[14], 4);
    assert_eq!(cpu.r[15], 8 + 8);
}

#[test]
fn bx_switches_to_thumb() {
    let mut bus = TestBus::new();
    bus.load(
        0,
        &[
            0xE3A0_0011, // mov r0, #0x11   (address 0x10, Thumb bit set)
            0xE12F_FF10, // bx r0
        ],
    );
    // At 0x10: movs r1, #0x2A
    bus.load16(0x10, &[0x212A]);
    let mut cpu = Cpu::new();
    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert!(cpu.thumb(), "should be in Thumb state");
    assert_eq!(cpu.r[1], 0x2A);
}

#[test]
fn thumb_long_branch_with_link() {
    let mut bus = TestBus::new();
    let mut cpu = Cpu::new();
    cpu.cpsr |= T_BIT;
    // bl +0x20 encoded as the usual two-halfword pair.
    bus.load16(0, &[0xF000, 0xF80E]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.r[15], 0x20);
    assert_eq!(cpu.r[14], 5); // return address with the Thumb bit
}

#[test]
fn multiply_long_signed() {
    let (cpu, _) = run(
        &[
            0xE3E0_0000, // mvn r0, #0        -> -1
            0xE3A0_1002, // mov r1, #2
            0xE0C3_2091, // smull r2, r3, r1, r0
        ],
        3,
    );
    let result = ((cpu.r[3] as u64) << 32 | cpu.r[2] as u64) as i64;
    assert_eq!(result, -2);
}

#[test]
fn swi_enters_supervisor_with_return_address() {
    let mut bus = TestBus::new();
    bus.load(0, &[0xEF00_0000]);
    let mut cpu = Cpu::new();
    cpu.set_mode(MODE_USR);
    cpu.step(&mut bus);
    assert_eq!(cpu.mode(), MODE_SVC);
    assert_eq!(cpu.r[14], 4);
    assert_eq!(cpu.r[15], 0x08);
    assert_eq!(cpu.spsr & MODE_MASK, MODE_USR);
    assert!(cpu.cpsr & I_BIT != 0, "IRQs masked on entry");
}

#[test]
fn irq_is_taken_between_instructions() {
    let mut bus = TestBus::new();
    bus.load(0, &[0xE1A0_0000, 0xE1A0_0000]);
    let mut cpu = Cpu::new();
    cpu.cpsr = MODE_SVC; // interrupts unmasked
    cpu.step(&mut bus);
    bus.irq = true;
    cpu.step(&mut bus);
    assert_eq!(cpu.mode(), MODE_IRQ);
    assert_eq!(cpu.r[15], 0x18);
    assert_eq!(cpu.r[14], 4 + 4);
}

#[test]
fn banked_registers_survive_mode_switches() {
    let mut cpu = Cpu::new();

    cpu.set_mode(MODE_SVC);
    cpu.r[13] = 0x1000;
    cpu.r[8] = 0xAAAA;

    cpu.set_mode(MODE_IRQ);
    cpu.r[13] = 0x2000;

    // FIQ banks r8..r12 on top of r13/r14.
    cpu.set_mode(MODE_FIQ);
    cpu.r[13] = 0x3000;
    cpu.r[8] = 0xFFFF;

    cpu.set_mode(MODE_SVC);
    assert_eq!(cpu.r[13], 0x1000);
    assert_eq!(cpu.r[8], 0xAAAA, "r8 is not banked outside FIQ");

    cpu.set_mode(MODE_IRQ);
    assert_eq!(cpu.r[13], 0x2000);
    assert_eq!(cpu.r[8], 0xAAAA);

    cpu.set_mode(MODE_FIQ);
    assert_eq!(cpu.r[13], 0x3000);
    assert_eq!(cpu.r[8], 0xFFFF);
}

#[test]
fn user_bank_is_shared_with_system_mode() {
    let mut cpu = Cpu::new();
    cpu.set_mode(MODE_USR);
    cpu.r[13] = 0x7000;
    cpu.set_mode(MODE_SYS);
    assert_eq!(cpu.r[13], 0x7000);
}

#[test]
fn mmu_section_translation() {
    let mut bus = TestBus::new();
    // One 1 MB section mapping VA 0x00300000 to PA 0x00200000, domain 0,
    // AP = 11 (full access). Section bases are 1 MB aligned.
    let l1_base = 0x4000u32;
    bus.write32(l1_base + (3 << 2), 0x0020_0C02);
    bus.write32(0x0020_0010, 0xDEAD_BEEF);

    let mut cpu = Cpu::new();
    cpu.cp15.ttbr = l1_base;
    cpu.cp15.dacr = 0x1; // domain 0 = client
    cpu.cp15.control |= crate::cp15::ctl::M;
    cpu.mmu_active = true;

    let v = cpu.read_u32(&mut bus, 0x0030_0010).expect("translation should succeed");
    assert_eq!(v, 0xDEAD_BEEF);

    // A second access to the same page must come back through the TLB.
    let v = cpu.read_u32(&mut bus, 0x0030_0010).unwrap();
    assert_eq!(v, 0xDEAD_BEEF);
}

#[test]
fn mmu_translation_fault_raises_data_abort() {
    let mut bus = TestBus::new();
    let mut cpu = Cpu::new();
    cpu.cp15.ttbr = 0x4000;
    cpu.cp15.dacr = 0x1;
    cpu.cp15.control |= crate::cp15::ctl::M;
    cpu.mmu_active = true;

    let err = cpu.read_u32(&mut bus, 0x0010_0010).unwrap_err();
    assert_eq!(err.fsr & 0xF, 0x5, "section translation fault");
}

#[test]
fn fcse_relocates_the_low_32mb() {
    let mut cpu = Cpu::new();
    cpu.cp15.pid = 0x0200_0000; // slot 1
    assert_eq!(cpu.cp15.fcse(0x0001_0000), 0x0201_0000);
    // Above 32 MB the mapping is the identity.
    assert_eq!(cpu.cp15.fcse(0x8000_0000), 0x8000_0000);
}

#[test]
fn high_vectors_move_the_exception_table() {
    let mut bus = TestBus::new();
    bus.load(0, &[0xEF00_0000]);
    let mut cpu = Cpu::new();
    cpu.cp15.control |= crate::cp15::ctl::V;
    cpu.step(&mut bus);
    assert_eq!(cpu.r[15], 0xFFFF_0008);
}

#[test]
fn small_page_subpages_have_independent_permissions() {
    // ARMv5 small pages carry four AP fields, one per 1 KB subpage. Windows
    // CE relies on it: the page holding PUserKData at 0xFFFFC800 is
    // privileged-only in its lower subpages and user-readable in the one the
    // shared structure lives in. A TLB with a 4 KB granule lets whichever
    // subpage is touched first dictate the permissions of its neighbours.
    let mut bus = TestBus::new();
    let l1_base = 0x4000u32;
    let l2_base = 0x5000u32;

    // VA 0x00300000..0x00400000 through a coarse table, domain 0.
    bus.write32(l1_base + (3 << 2), l2_base | 0x01);
    // One small page at VA 0x0030C000 -> PA 0x00200000, with per-subpage AP:
    //   subpage 0,1 = 01 (privileged only), 2 = 10 (user read-only), 3 = 01.
    let ap = (0b01 << 4) | (0b01 << 6) | (0b10 << 8) | (0b01 << 10);
    bus.write32(l2_base + ((0x0C) << 2), 0x0020_0000 | ap | 0x02);
    bus.write32(0x0020_0800, 0xCAFE_F00D);

    let mut cpu = Cpu::new();
    cpu.cp15.ttbr = l1_base;
    cpu.cp15.dacr = 0x1; // domain 0 = client
    cpu.cp15.control |= crate::cp15::ctl::M;
    cpu.mmu_active = true;

    // Privileged access to subpage 1 first, which is the one CE touches.
    cpu.set_mode(MODE_SVC);
    cpu.read_u32(&mut bus, 0x0030_C400).expect("privileged read of subpage 1");

    // Now a User-mode read of subpage 2 must still be allowed.
    cpu.set_mode(MODE_USR);
    let v = cpu
        .read_u32(&mut bus, 0x0030_C800)
        .expect("user read of a user-readable subpage");
    assert_eq!(v, 0xCAFE_F00D);

    // And a User-mode read of subpage 1 must still be denied.
    let err = cpu.read_u32(&mut bus, 0x0030_C400).unwrap_err();
    assert_eq!(err.fsr & 0xF, 0xF, "page permission fault");
}

#[test]
fn tiny_pages_resolve_to_their_own_kilobyte() {
    // Four tiny pages in one 4 KB span, each mapped somewhere different.
    let mut bus = TestBus::new();
    let (l1_base, l2_base) = (0x4000u32, 0x8000u32);
    bus.write32(l1_base + (3 << 2), l2_base | 0x03); // fine table
    for i in 0..4u32 {
        let pa = 0x0020_0000 + i * 0x400;
        // Fine tables are indexed by VA bits 19:10.
        let idx = i;
        bus.write32(l2_base + (idx << 2), pa | (0b11 << 4) | 0x03);
        bus.write32(pa, 0x1000 + i);
    }

    let mut cpu = Cpu::new();
    cpu.cp15.ttbr = l1_base;
    cpu.cp15.dacr = 0x1;
    cpu.cp15.control |= crate::cp15::ctl::M;
    cpu.mmu_active = true;

    for i in 0..4u32 {
        let v = cpu.read_u32(&mut bus, 0x0030_0000 + i * 0x400).unwrap();
        assert_eq!(v, 0x1000 + i, "tiny page {i} must map to its own kilobyte");
    }
}

#[test]
fn power_mode_halts_the_core_and_an_interrupt_wakes_it() {
    let mut bus = TestBus::new();
    // mcr p14, 0, r0, c7, c0, 0  with r0 = 1 (idle), then two no-ops.
    bus.load(0, &[0xE3A0_0001, 0xEE07_0E10, 0xE1A0_0000, 0xE1A0_0000]);
    let mut cpu = Cpu::new();
    cpu.cpsr = MODE_SVC | I_BIT; // interrupts masked, as CE idles

    cpu.step(&mut bus); // mov
    cpu.step(&mut bus); // mcr -> halt
    assert!(cpu.halted, "a non-zero power mode halts the core");
    assert_eq!(cpu.pwrmode, 1);

    let pc = cpu.r[15];
    cpu.step(&mut bus);
    assert_eq!(cpu.r[15], pc, "a halted core does not advance");

    // Asserting the line wakes it even though CPSR still masks interrupts.
    bus.irq = true;
    cpu.step(&mut bus);
    assert!(!cpu.halted, "the interrupt line wakes the core regardless of CPSR");
    assert_eq!(cpu.mode(), MODE_SVC, "but the exception is not taken while masked");
}
