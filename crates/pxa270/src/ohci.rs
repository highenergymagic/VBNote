//! The USB host controller: OHCI, plus the PXA270's own wrapper around it.
//!
//! This is the way files get on and off the machine. The firmware has the
//! whole stack -- `ohci.dll`, `usbd.dll`, `usbmsc.dll`, `usbdisk6.dll` -- and
//! the OHCI driver is this board's own, built from
//! `platform\gandalf\drivers\usb\ohcd\ohcdpdd\ohcdpdd.cpp`. On a boot it
//! already resets the controller, hands it an HCCA in SDRAM, sets frame
//! timing, enables the master interrupt and powers the root hub, then asks
//! how many ports it has and stops.
//!
//! It stops because `HcRhDescriptorA` used to read back as zero, which says
//! **the root hub has no downstream ports**. Nothing was wrong with the
//! driver; it was told there was nowhere to plug anything in.
//!
//! # Two register sets at one base
//!
//! `0x4C00_0000` carries the standard OHCI operational registers at their
//! usual offsets, and then three of Intel's own above them:
//!
//! | offset | |
//! |---|---|
//! | `0x00`-`0x5C` | OHCI: control, status, list heads, frame timing, root hub |
//! | `0x64` | `UHCHR`, the PXA host controller reset and power control |
//! | `0x68` | `UHCHIE`, PXA interrupt enable |
//! | `0x6C` | `UHCHIT`, PXA interrupt test |
//!
//! `UHCHR` is the one to know about: the driver reads and writes it more than
//! anything else during bring-up, and a first reading of a boot log that
//! assumed the standard map alone mistook it for a port status register.
//!
//! # What "a device appeared" means here
//!
//! `HcRhPortStatus[n]` bit 0 is CCS, current connect status. Setting it and
//! raising the root-hub-status-change interrupt is the whole of insertion --
//! a published mechanism inside this controller, with no board-specific
//! signal to reverse-engineer. That is why this route was chosen over the
//! PCMCIA socket, whose driver asks for nothing at all.

use std::collections::BTreeMap;

use crate::intc::{self, Intc};

pub const BASE: u32 = 0x4C00_0000;

/// How many downstream ports the root hub reports. The machine has two USB
/// host ports, and `ohci.dll` claims an interrupt for each (IRQ 3 and IRQ 2).
pub const PORTS: u32 = 2;

// Standard OHCI operational registers.
const HC_REVISION: u32 = 0x00;
const HC_CONTROL: u32 = 0x04;
const HC_COMMAND_STATUS: u32 = 0x08;
const HC_INTERRUPT_STATUS: u32 = 0x0C;
const HC_INTERRUPT_ENABLE: u32 = 0x10;
const HC_INTERRUPT_DISABLE: u32 = 0x14;
const HC_HCCA: u32 = 0x18;
const HC_PERIOD_CURRENT_ED: u32 = 0x1C;
const HC_CONTROL_HEAD_ED: u32 = 0x20;
const HC_CONTROL_CURRENT_ED: u32 = 0x24;
const HC_BULK_HEAD_ED: u32 = 0x28;
const HC_BULK_CURRENT_ED: u32 = 0x2C;
const HC_DONE_HEAD: u32 = 0x30;
const HC_FM_INTERVAL: u32 = 0x34;
const HC_FM_REMAINING: u32 = 0x38;
const HC_FM_NUMBER: u32 = 0x3C;
const HC_PERIODIC_START: u32 = 0x40;
const HC_LS_THRESHOLD: u32 = 0x44;
const HC_RH_DESCRIPTOR_A: u32 = 0x48;
const HC_RH_DESCRIPTOR_B: u32 = 0x4C;
const HC_RH_STATUS: u32 = 0x50;
const HC_RH_PORT_STATUS: u32 = 0x54;

// The PXA270's own registers above the OHCI block.
const UHCHR: u32 = 0x64;
const UHCHIE: u32 = 0x68;
const UHCHIT: u32 = 0x6C;

/// `HcInterruptStatus` / `HcInterruptEnable` bits, the ones used here.
const INT_WRITEBACK_DONE: u32 = 1 << 1;
const INT_START_OF_FRAME: u32 = 1 << 2;
const INT_ROOT_HUB_CHANGE: u32 = 1 << 6;
const INT_MASTER_ENABLE: u32 = 1 << 31;

/// `HcRhPortStatus` bits.
const PORT_CONNECTED: u32 = 1 << 0;
const PORT_ENABLED: u32 = 1 << 1;
const PORT_SUSPENDED: u32 = 1 << 2;
const PORT_RESET: u32 = 1 << 4;
const PORT_POWERED: u32 = 1 << 8;
const PORT_LOW_SPEED: u32 = 1 << 9;
/// Write-one-to-clear change bits, in the top half.
const PORT_CONNECT_CHANGE: u32 = 1 << 16;
const PORT_ENABLE_CHANGE: u32 = 1 << 17;
const PORT_SUSPEND_CHANGE: u32 = 1 << 18;
const PORT_OVERCURRENT_CHANGE: u32 = 1 << 19;
const PORT_RESET_CHANGE: u32 = 1 << 20;

/// `UHCHR` bits that the driver drives during bring-up.
const UHCHR_FSBIR: u32 = 1 << 0;
const UHCHR_FHR: u32 = 1 << 1;
const UHCHR_SSE: u32 = 1 << 5;
const UHCHR_SSEP0: u32 = 1 << 6;

/// One downstream port of the root hub.
#[derive(Default, Clone, Copy)]
pub struct Port {
    pub status: u32,
}

pub struct Ohci {
    pub control: u32,
    pub command_status: u32,
    interrupt_status: u32,
    interrupt_enable: u32,
    pub hcca: u32,
    period_current_ed: u32,
    pub control_head_ed: u32,
    control_current_ed: u32,
    pub bulk_head_ed: u32,
    bulk_current_ed: u32,
    pub done_head: u32,
    fm_interval: u32,
    pub fm_number: u32,
    periodic_start: u32,
    ls_threshold: u32,
    rh_status: u32,
    pub ports: [Port; PORTS as usize],
    uhchr: u32,
    uhchie: u32,
    uhchit: u32,
    /// Registers outside the map, kept for the same reason the rest of the
    /// SoC keeps them: an unexpected access should be visible, not silent.
    pub unexpected: BTreeMap<u32, u32>,
}

impl Default for Ohci {
    fn default() -> Self {
        Ohci::new()
    }
}

impl Ohci {
    pub fn new() -> Ohci {
        Ohci {
            control: 0,
            command_status: 0,
            interrupt_status: 0,
            interrupt_enable: 0,
            hcca: 0,
            period_current_ed: 0,
            control_head_ed: 0,
            control_current_ed: 0,
            bulk_head_ed: 0,
            bulk_current_ed: 0,
            done_head: 0,
            // 11999 frame interval and the usual largest packet, which is
            // what the driver writes back anyway.
            fm_interval: 0x2EDF,
            fm_number: 0,
            periodic_start: 0,
            ls_threshold: 0x0628,
            rh_status: 0,
            // Powered from reset is wrong; the driver turns power on itself
            // through HcRhStatus, and a port that claims power before it is
            // given makes that step untestable.
            ports: [Port::default(); PORTS as usize],
            uhchr: UHCHR_FHR | UHCHR_SSE | UHCHR_SSEP0,
            uhchie: 0,
            uhchit: 0,
            unexpected: BTreeMap::new(),
        }
    }

    /// Say a device has appeared on `port`, or gone away.
    ///
    /// Nothing else is needed to make the guest look: CCS changes, the change
    /// bit latches, and the root hub raises its interrupt. What the driver
    /// does next -- reset, enumerate, ask for descriptors -- is its own.
    pub fn set_connected(&mut self, port: usize, connected: bool, intc: &mut Intc) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        let was = p.status & PORT_CONNECTED != 0;
        if was == connected {
            return;
        }
        if connected {
            p.status |= PORT_CONNECTED;
        } else {
            p.status &= !(PORT_CONNECTED | PORT_ENABLED);
        }
        p.status |= PORT_CONNECT_CHANGE;
        self.raise(INT_ROOT_HUB_CHANGE, intc);
    }

    pub fn connected(&self, port: usize) -> bool {
        self.ports
            .get(port)
            .is_some_and(|p| p.status & PORT_CONNECTED != 0)
    }

    /// Raise an interrupt from the board-side list walker, which is where
    /// transfers actually happen.
    pub fn raise_from_board(&mut self, bits: u32, intc: &mut Intc) {
        self.raise(bits, intc);
    }

    fn raise(&mut self, bits: u32, intc: &mut Intc) {
        self.interrupt_status |= bits;
        self.update(intc);
    }

    /// The line follows status AND enable, with the master enable on top.
    fn update(&mut self, intc: &mut Intc) {
        let pending = self.interrupt_status & self.interrupt_enable != 0
            && self.interrupt_enable & INT_MASTER_ENABLE != 0;
        intc.set(intc::IRQ_USB_HOST_1, pending);
    }

    fn rh_descriptor_a(&self) -> u32 {
        // NDP in the low byte -- the number the driver was reading as zero,
        // which is why it never went looking for a device. NPS clear and PSM
        // set: ports are powered together, which is what the driver's single
        // write to HcRhStatus expects.
        PORTS | (1 << 8)
    }

    pub fn read(&mut self, off: u32, intc: &mut Intc) -> u32 {
        let _ = intc;
        match off {
            // OHCI 1.0.
            HC_REVISION => 0x10,
            HC_CONTROL => self.control,
            HC_COMMAND_STATUS => self.command_status,
            HC_INTERRUPT_STATUS => self.interrupt_status,
            HC_INTERRUPT_ENABLE | HC_INTERRUPT_DISABLE => self.interrupt_enable,
            HC_HCCA => self.hcca,
            HC_PERIOD_CURRENT_ED => self.period_current_ed,
            HC_CONTROL_HEAD_ED => self.control_head_ed,
            HC_CONTROL_CURRENT_ED => self.control_current_ed,
            HC_BULK_HEAD_ED => self.bulk_head_ed,
            HC_BULK_CURRENT_ED => self.bulk_current_ed,
            HC_DONE_HEAD => self.done_head,
            HC_FM_INTERVAL => self.fm_interval,
            // Nothing here counts down within a frame, and a driver that
            // waits for a particular remaining count would hang; reporting
            // the full interval means "the frame has just begun".
            HC_FM_REMAINING => self.fm_interval & 0x3FFF,
            HC_FM_NUMBER => self.fm_number,
            HC_PERIODIC_START => self.periodic_start,
            HC_LS_THRESHOLD => self.ls_threshold,
            HC_RH_DESCRIPTOR_A => self.rh_descriptor_a(),
            HC_RH_DESCRIPTOR_B => 0,
            HC_RH_STATUS => self.rh_status,
            _ if self.port_of(off).is_some() => {
                let n = self.port_of(off).unwrap();
                self.ports[n].status
            }
            UHCHR => self.uhchr,
            UHCHIE => self.uhchie,
            UHCHIT => self.uhchit,
            _ => {
                self.unexpected.entry(off).or_insert(0);
                0
            }
        }
    }

    fn port_of(&self, off: u32) -> Option<usize> {
        if (HC_RH_PORT_STATUS..HC_RH_PORT_STATUS + PORTS * 4).contains(&off) {
            Some(((off - HC_RH_PORT_STATUS) / 4) as usize)
        } else {
            None
        }
    }

    pub fn write(&mut self, off: u32, val: u32, intc: &mut Intc) {
        match off {
            HC_CONTROL => self.control = val,
            HC_COMMAND_STATUS => {
                // HCR: a host controller reset. It clears the lists and the
                // interrupt state but leaves the root hub alone, so a device
                // already plugged in stays plugged in.
                if val & 1 != 0 {
                    self.control = 0;
                    self.interrupt_status = 0;
                    self.interrupt_enable = 0;
                    self.done_head = 0;
                }
                // The list-filled bits are write-only doorbells.
                self.command_status = val & !0x0000_0003;
                self.update(intc);
            }
            // Both are write-one-to-clear or write-one-to-set; neither is a
            // plain store, and treating them as one leaves an interrupt
            // asserted for ever.
            HC_INTERRUPT_STATUS => {
                self.interrupt_status &= !val;
                self.update(intc);
            }
            HC_INTERRUPT_ENABLE => {
                self.interrupt_enable |= val;
                self.update(intc);
            }
            HC_INTERRUPT_DISABLE => {
                self.interrupt_enable &= !val;
                self.update(intc);
            }
            HC_HCCA => self.hcca = val & !0xFF,
            HC_PERIOD_CURRENT_ED => self.period_current_ed = val,
            HC_CONTROL_HEAD_ED => self.control_head_ed = val,
            HC_CONTROL_CURRENT_ED => self.control_current_ed = val,
            HC_BULK_HEAD_ED => self.bulk_head_ed = val,
            HC_BULK_CURRENT_ED => self.bulk_current_ed = val,
            HC_DONE_HEAD => self.done_head = val,
            HC_FM_INTERVAL => self.fm_interval = val,
            HC_FM_NUMBER => self.fm_number = val,
            HC_PERIODIC_START => self.periodic_start = val,
            HC_LS_THRESHOLD => self.ls_threshold = val,
            HC_RH_DESCRIPTOR_A | HC_RH_DESCRIPTOR_B => {}
            HC_RH_STATUS => {
                // LPSC, set global power: every port comes up powered.
                if val & (1 << 16) != 0 {
                    for p in self.ports.iter_mut() {
                        p.status |= PORT_POWERED;
                    }
                }
                // LPS, clear global power.
                if val & 1 != 0 {
                    for p in self.ports.iter_mut() {
                        p.status &= !PORT_POWERED;
                    }
                }
            }
            _ if self.port_of(off).is_some() => {
                let n = self.port_of(off).unwrap();
                self.write_port(n, val, intc);
            }
            UHCHR => {
                self.uhchr = val;
                // FHR is a force host reset: held, the controller is in
                // reset; released, it comes back. The driver pulses it.
                if val & UHCHR_FHR == 0 && val & UHCHR_FSBIR != 0 {
                    self.uhchr &= !UHCHR_FSBIR;
                }
            }
            UHCHIE => self.uhchie = val,
            UHCHIT => self.uhchit = val,
            _ => {
                self.unexpected.insert(off, val);
            }
        }
    }

    fn write_port(&mut self, n: usize, val: u32, intc: &mut Intc) {
        let connected = self.ports[n].status & PORT_CONNECTED != 0;
        let p = &mut self.ports[n];

        // Clearing the change bits is write-one-to-clear.
        p.status &= !(val
            & (PORT_CONNECT_CHANGE
                | PORT_ENABLE_CHANGE
                | PORT_SUSPEND_CHANGE
                | PORT_OVERCURRENT_CHANGE
                | PORT_RESET_CHANGE));

        // Set port enable.
        if val & (1 << 1) != 0 && connected {
            p.status |= PORT_ENABLED;
        }
        // Clear port enable.
        if val & (1 << 0) != 0 {
            p.status &= !PORT_ENABLED;
        }
        // Set suspend, clear suspend.
        if val & (1 << 2) != 0 {
            p.status |= PORT_SUSPENDED;
        }
        if val & (1 << 3) != 0 {
            p.status &= !PORT_SUSPENDED;
        }
        // Set port reset. A real reset takes ~10 ms and finishes by itself;
        // here it completes at once, leaving the port enabled and the reset
        // change bit set, which is what the driver waits for.
        if val & PORT_RESET != 0 && connected {
            p.status &= !PORT_RESET;
            p.status |= PORT_ENABLED | PORT_RESET_CHANGE;
            self.raise(INT_ROOT_HUB_CHANGE, intc);
            return;
        }
        // Set port power, clear port power.
        if val & PORT_POWERED != 0 {
            p.status |= PORT_POWERED;
        }
        if val & (1 << 9) != 0 {
            p.status &= !PORT_POWERED;
        }
        let _ = PORT_LOW_SPEED;
        let _ = INT_WRITEBACK_DONE;
        let _ = INT_START_OF_FRAME;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts() -> (Ohci, Intc) {
        (Ohci::new(), Intc::default())
    }

    /// The whole reason USB was chosen over the PCMCIA socket: the driver
    /// asked how many ports it had, was told none, and correctly stopped.
    #[test]
    fn the_root_hub_reports_its_ports() {
        let (mut hc, mut intc) = parts();
        assert_eq!(hc.read(HC_RH_DESCRIPTOR_A, &mut intc) & 0xFF, PORTS);
        assert_ne!(PORTS, 0, "a root hub with no ports is what stalled this");
    }

    #[test]
    fn it_identifies_as_ohci_revision_one() {
        let (mut hc, mut intc) = parts();
        assert_eq!(hc.read(HC_REVISION, &mut intc), 0x10);
    }

    /// UHCHR is the PXA's own, above the OHCI block. A boot log read against
    /// the standard map alone mistakes 0x64 for port status.
    #[test]
    fn the_pxa_registers_are_not_port_status() {
        let (mut hc, mut intc) = parts();
        assert!(hc.port_of(UHCHR).is_none());
        assert!(hc.port_of(HC_RH_PORT_STATUS).is_some());
        assert!(hc.port_of(HC_RH_PORT_STATUS + 4).is_some());
        assert!(hc.port_of(HC_RH_PORT_STATUS + PORTS * 4).is_none());
        hc.write(UHCHR, UHCHR_SSE, &mut intc);
        assert_eq!(hc.read(UHCHR, &mut intc), UHCHR_SSE);
    }

    #[test]
    fn power_comes_from_the_driver_not_from_reset() {
        let (mut hc, mut intc) = parts();
        assert_eq!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_POWERED, 0);
        hc.write(HC_RH_STATUS, 1 << 16, &mut intc);
        assert_ne!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_POWERED, 0);
    }

    /// Plugging something in is a status bit and an interrupt, and nothing
    /// else. No board signal, which is the point of this route.
    #[test]
    fn plugging_in_raises_the_root_hub_interrupt() {
        let (mut hc, mut intc) = parts();
        hc.write(HC_INTERRUPT_ENABLE, INT_MASTER_ENABLE | INT_ROOT_HUB_CHANGE, &mut intc);
        assert!(!intc.is_pending(intc::IRQ_USB_HOST_1));

        hc.set_connected(0, true, &mut intc);
        let status = hc.read(HC_RH_PORT_STATUS, &mut intc);
        assert_ne!(status & PORT_CONNECTED, 0);
        assert_ne!(status & PORT_CONNECT_CHANGE, 0);
        assert!(intc.is_pending(intc::IRQ_USB_HOST_1), "no interrupt for a connect");
    }

    /// An interrupt that cannot be cleared is worse than one never raised:
    /// the guest spins in its handler for ever.
    #[test]
    fn the_status_register_is_write_one_to_clear() {
        let (mut hc, mut intc) = parts();
        hc.write(HC_INTERRUPT_ENABLE, INT_MASTER_ENABLE | INT_ROOT_HUB_CHANGE, &mut intc);
        hc.set_connected(0, true, &mut intc);
        assert!(intc.is_pending(intc::IRQ_USB_HOST_1));

        hc.write(HC_INTERRUPT_STATUS, INT_ROOT_HUB_CHANGE, &mut intc);
        assert!(!intc.is_pending(intc::IRQ_USB_HOST_1));
    }

    #[test]
    fn the_change_bits_clear_by_writing_one() {
        let (mut hc, mut intc) = parts();
        hc.set_connected(0, true, &mut intc);
        assert_ne!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_CONNECT_CHANGE, 0);
        hc.write(HC_RH_PORT_STATUS, PORT_CONNECT_CHANGE, &mut intc);
        assert_eq!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_CONNECT_CHANGE, 0);
        // And the device is still there afterwards.
        assert_ne!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_CONNECTED, 0);
    }

    /// Resetting a port is how the driver gets to talk to a device. It has
    /// to finish, and leave the port enabled, or enumeration never starts.
    #[test]
    fn a_port_reset_completes_and_enables() {
        let (mut hc, mut intc) = parts();
        hc.set_connected(0, true, &mut intc);
        hc.write(HC_RH_PORT_STATUS, PORT_RESET, &mut intc);
        let status = hc.read(HC_RH_PORT_STATUS, &mut intc);
        assert_eq!(status & PORT_RESET, 0, "reset never finished");
        assert_ne!(status & PORT_ENABLED, 0, "port not enabled after reset");
        assert_ne!(status & PORT_RESET_CHANGE, 0);
    }

    /// Resetting an empty port must not enable it, or the driver goes off
    /// enumerating a device that is not there.
    #[test]
    fn resetting_an_empty_port_does_nothing() {
        let (mut hc, mut intc) = parts();
        hc.write(HC_RH_PORT_STATUS, PORT_RESET, &mut intc);
        assert_eq!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_ENABLED, 0);
    }

    /// A controller reset clears the lists but must not unplug anything.
    #[test]
    fn a_controller_reset_leaves_the_device_plugged_in() {
        let (mut hc, mut intc) = parts();
        hc.write(HC_RH_STATUS, 1 << 16, &mut intc);
        hc.set_connected(0, true, &mut intc);
        hc.write(HC_COMMAND_STATUS, 1, &mut intc);
        assert_ne!(hc.read(HC_RH_PORT_STATUS, &mut intc) & PORT_CONNECTED, 0);
        assert!(!intc.is_pending(intc::IRQ_USB_HOST_1));
    }

    #[test]
    fn unplugging_clears_connect_and_enable() {
        let (mut hc, mut intc) = parts();
        hc.set_connected(0, true, &mut intc);
        hc.write(HC_RH_PORT_STATUS, PORT_RESET, &mut intc);
        hc.write(HC_RH_PORT_STATUS, PORT_CONNECT_CHANGE | PORT_RESET_CHANGE, &mut intc);
        assert!(hc.connected(0));

        hc.set_connected(0, false, &mut intc);
        let status = hc.read(HC_RH_PORT_STATUS, &mut intc);
        assert_eq!(status & PORT_CONNECTED, 0);
        assert_eq!(status & PORT_ENABLED, 0);
        assert_ne!(status & PORT_CONNECT_CHANGE, 0);
    }
}
