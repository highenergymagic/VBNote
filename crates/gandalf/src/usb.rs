//! Walking OHCI's descriptor lists, and the device on the other end.
//!
//! The register file lives in `pxa270::ohci`; the transfers happen here,
//! because a transfer moves data between SDRAM and a device and only the board
//! can reach both. The same split as `dma.rs`, for the same reason.
//!
//! # What the controller is asked to do
//!
//! The driver builds two linked structures in memory and points the controller
//! at them:
//!
//! - an **endpoint descriptor** (ED) per endpoint, four words: who to talk to,
//!   a head pointer and a tail pointer, and the next ED. Head equal to tail
//!   means nothing to do.
//! - **transfer descriptors** (TD) chained from the head, four words each:
//!   which kind of packet, where the buffer is, where it ends, and the next TD.
//!
//! Running a TD means moving its buffer to or from the device, writing the
//! result back into the TD, unlinking it from the ED, and pushing it onto the
//! done queue. When the queue is handed over -- through `HccaDoneHead`, with
//! the writeback interrupt -- the driver knows its transfers finished.
//!
//! # The two details that corrupt data rather than failing
//!
//! **A buffer may straddle a page.** OHCI allows the current buffer pointer
//! and the buffer end to sit in different 4 KB pages, and then the transfer is
//! not the range between them: it is the rest of the first page followed by
//! the start of the last one. Treating it as contiguous reads whatever
//! happens to lie in between, which is somebody else's memory, and the
//! transfer still reports success.
//!
//! **The done queue is a stack.** A TD is pushed by pointing it at the current
//! head, so the queue comes out in reverse order of completion. Drivers know
//! this; one built the other way round quietly reverses the driver's idea of
//! which transfer finished.

use crate::Gandalf;
use arm::Bus;

/// Offsets into the HCCA, the 256-byte block the driver shares with the
/// controller.
const HCCA_FRAME_NUMBER: u32 = 0x80;
const HCCA_DONE_HEAD: u32 = 0x84;

/// `HcControl` bits.
const CONTROL_LIST_ENABLE: u32 = 1 << 4;
const BULK_LIST_ENABLE: u32 = 1 << 5;
/// The controller is running, rather than reset, suspended or resuming.
const FUNCTIONAL_STATE_OPERATIONAL: u32 = 2 << 6;

/// `HcCommandStatus` doorbells: the driver rings one after adding a TD.
const CONTROL_LIST_FILLED: u32 = 1 << 1;
const BULK_LIST_FILLED: u32 = 1 << 2;

/// `HcInterruptStatus` bit 1, writeback done head.
const INT_WRITEBACK_DONE: u32 = 1 << 1;

/// TD direction, in `dword0` bits 19:20.
const PID_SETUP: u32 = 0;
const PID_OUT: u32 = 1;
const PID_IN: u32 = 2;

/// Condition codes, `dword0` bits 28:31.
const CC_NO_ERROR: u32 = 0;
const CC_STALL: u32 = 4;

/// How many transfer descriptors to run in one service call.
///
/// A cap rather than "until the lists are empty": a driver that leaves a TD
/// pointing at itself would otherwise stall the emulator entirely, and this
/// runs inside the board's tick.
const MAX_TDS: u32 = 64;

/// An endpoint descriptor, as read out of memory.
struct Ed {
    at: u32,
    control: u32,
    tail: u32,
    /// The whole word, low bits and all: bit 0 is halted, bit 1 is the
    /// toggle carry, and they have to be written back with the pointer.
    head_word: u32,
    next: u32,
}

impl Ed {
    fn read(board: &mut Gandalf, at: u32) -> Ed {
        Ed {
            at,
            control: board.read32(at),
            tail: board.read32(at + 4) & !0xF,
            head_word: board.read32(at + 8),
            next: board.read32(at + 12) & !0xF,
        }
    }

    fn head(&self) -> u32 {
        self.head_word & !0xF
    }

    fn halted(&self) -> bool {
        self.head_word & 1 != 0
    }

    fn skipped(&self) -> bool {
        self.control & (1 << 14) != 0
    }

    fn address(&self) -> u8 {
        (self.control & 0x7F) as u8
    }

    fn endpoint(&self) -> u8 {
        ((self.control >> 7) & 0xF) as u8
    }
}

/// The pieces of a transfer descriptor that matter here.
struct Td {
    at: u32,
    dword0: u32,
    buffer: u32,
    next: u32,
    end: u32,
}

impl Td {
    fn read(board: &mut Gandalf, at: u32) -> Td {
        Td {
            at,
            dword0: board.read32(at),
            buffer: board.read32(at + 4),
            next: board.read32(at + 8) & !0xF,
            end: board.read32(at + 12),
        }
    }

    fn pid(&self) -> u32 {
        (self.dword0 >> 19) & 3
    }

    /// Every byte of the buffer, in order, as physical addresses.
    ///
    /// This is where the page rule lives. An empty buffer -- a status stage,
    /// or a zero-length packet -- has no current buffer pointer at all.
    fn addresses(&self) -> Vec<u32> {
        if self.buffer == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.buffer & !0xFFF == self.end & !0xFFF {
            for a in self.buffer..=self.end {
                out.push(a);
            }
        } else {
            let first_page_end = (self.buffer & !0xFFF) | 0xFFF;
            for a in self.buffer..=first_page_end {
                out.push(a);
            }
            let last_page = self.end & !0xFFF;
            for a in last_page..=self.end {
                out.push(a);
            }
        }
        out
    }
}

/// What a device does when spoken to. Implemented by whatever is plugged in.
pub trait Device {
    /// A control transfer: the eight setup bytes, and any data the host sent.
    /// Returns the data to give back, or `None` to stall.
    fn control(&mut self, setup: &[u8; 8], data_out: &[u8]) -> Option<Vec<u8>>;

    /// The host wants up to `len` bytes from a bulk endpoint.
    fn bulk_in(&mut self, endpoint: u8, len: usize) -> Vec<u8>;

    /// The host is sending bytes to a bulk endpoint.
    fn bulk_out(&mut self, endpoint: u8, data: &[u8]);

    /// The address the device has been told to answer to. Zero until the host
    /// assigns one, which it does with a control transfer to address zero.
    fn address(&self) -> u8;

    /// How many commands the device has been given over its bulk endpoints.
    ///
    /// Enumeration proves the controller works. This proves the *class*
    /// driver bound and is using it, which is a different question and the
    /// one that decides whether a volume ever appears.
    fn commands(&self) -> u64 {
        0
    }
}

/// Run some of the controller's work. Called from the board's tick.
///
/// Returns whether anything was done, so the caller can stop asking.
pub fn service(board: &mut Gandalf) -> bool {
    if board.usb.is_none() {
        return false;
    }
    let control = board.soc.ohci.control;
    if control & 0xC0 != FUNCTIONAL_STATE_OPERATIONAL {
        return false;
    }

    // The frame counter is what a driver watches to know the controller is
    // alive at all, and it is shared through the HCCA as well as the register.
    board.soc.ohci.fm_number = board.soc.ohci.fm_number.wrapping_add(1);
    let hcca = board.soc.ohci.hcca;
    if hcca != 0 {
        let frame = board.soc.ohci.fm_number as u16;
        let word = board.read32(hcca + HCCA_FRAME_NUMBER);
        board.write32(hcca + HCCA_FRAME_NUMBER, (word & 0xFFFF_0000) | frame as u32);
    }

    let mut ran = 0;
    if control & CONTROL_LIST_ENABLE != 0 {
        ran += run_list(board, board.soc.ohci.control_head_ed, MAX_TDS);
    }
    if control & BULK_LIST_ENABLE != 0 {
        ran += run_list(board, board.soc.ohci.bulk_head_ed, MAX_TDS - ran.min(MAX_TDS));
    }

    // The doorbells are acknowledged whether or not there was anything behind
    // them: they mean "look again", and this has looked.
    board.soc.ohci.command_status &= !(CONTROL_LIST_FILLED | BULK_LIST_FILLED);

    if ran > 0 {
        hand_over_done_queue(board);
    }
    ran > 0
}

/// Walk one list of endpoint descriptors, running what is queued on each.
fn run_list(board: &mut Gandalf, head: u32, budget: u32) -> u32 {
    let mut ed_at = head;
    let mut ran = 0;
    // Bounded: a circular ED list is legal and normal, so this must not be
    // "until the next pointer is null".
    let mut guard = 0;
    while ed_at != 0 && ran < budget && guard < 64 {
        guard += 1;
        let ed = Ed::read(board, ed_at);
        ed_at = ed.next;
        if ed.skipped() || ed.halted() {
            continue;
        }
        // Head equal to tail is an endpoint with nothing queued, which is
        // most of them most of the time.
        while ed.head() != ed.tail && ran < budget {
            let ed = Ed::read(board, ed.at);
            if ed.halted() || ed.head() == ed.tail {
                break;
            }
            run_td(board, &ed);
            ran += 1;
        }
    }
    ran
}

/// Run the transfer at the head of an endpoint, and retire it.
fn run_td(board: &mut Gandalf, ed: &Ed) {
    let td = Td::read(board, ed.head());
    let addresses = td.addresses();

    let mut stalled = false;
    match td.pid() {
        PID_SETUP => {
            // Eight bytes, and they are the only thing that says what the
            // rest of the transfer means, so they are remembered until the
            // data and status stages have been through.
            let mut setup = [0u8; 8];
            for (i, byte) in setup.iter_mut().enumerate() {
                *byte = board.read8(td.buffer + i as u32);
            }
            board.usb_setup = Some(setup);
            board.usb_reply = None;
        }
        PID_IN => {
            let want = addresses.len();
            let data = if ed.endpoint() == 0 {
                // The data stage of a control transfer. The device answered
                // when the setup packet arrived; this hands it over, a packet
                // at a time if the driver asked for it that way.
                let reply = take_control_reply(board, want);
                match reply {
                    Some(d) => d,
                    None => {
                        stalled = true;
                        Vec::new()
                    }
                }
            } else if let Some(dev) = board.usb.as_mut() {
                dev.bulk_in(ed.endpoint(), want)
            } else {
                Vec::new()
            };
            for (addr, byte) in addresses.iter().zip(data.iter()) {
                board.write8(*addr, *byte);
            }
        }
        PID_OUT => {
            let mut data = Vec::with_capacity(addresses.len());
            for addr in &addresses {
                data.push(board.read8(*addr));
            }
            if ed.endpoint() == 0 {
                // Either the data stage of a control write, or the status
                // stage of a control read. A zero-length one is the status.
                if !data.is_empty() {
                    board.usb_data_out.extend_from_slice(&data);
                }
                if data.is_empty() {
                    finish_control(board);
                }
            } else if let Some(dev) = board.usb.as_mut() {
                dev.bulk_out(ed.endpoint(), &data);
            }
        }
        _ => {}
    }

    // A control transfer's status stage in the other direction also ends it.
    if ed.endpoint() == 0 && td.pid() == PID_IN && addresses.is_empty() {
        finish_control(board);
    }

    // The setup packet is answered as soon as it arrives, so the data stage
    // has something to hand over.
    if td.pid() == PID_SETUP {
        answer_setup(board, ed.address());
    }

    retire(board, ed, &td, stalled);
}

/// Ask the device about the setup packet just received.
fn answer_setup(board: &mut Gandalf, _to: u8) {
    let Some(setup) = board.usb_setup else {
        return;
    };
    board.usb_data_out.clear();
    // A host-to-device transfer has its data still to come, so the device is
    // not asked until the status stage.
    if setup[0] & 0x80 == 0 && u16::from_le_bytes([setup[6], setup[7]]) > 0 {
        return;
    }
    if let Some(dev) = board.usb.as_mut() {
        board.usb_reply = dev.control(&setup, &[]);
    }
}

/// The status stage: a host-to-device transfer is delivered here, once all of
/// its data has arrived.
fn finish_control(board: &mut Gandalf) {
    let Some(setup) = board.usb_setup.take() else {
        return;
    };
    if setup[0] & 0x80 == 0 && !board.usb_data_out.is_empty() {
        let data = std::mem::take(&mut board.usb_data_out);
        if let Some(dev) = board.usb.as_mut() {
            dev.control(&setup, &data);
        }
    }
    board.usb_reply = None;
    board.usb_data_out.clear();
}

/// Hand over up to `want` bytes of the device's answer.
fn take_control_reply(board: &mut Gandalf, want: usize) -> Option<Vec<u8>> {
    let reply = board.usb_reply.as_mut()?;
    let n = want.min(reply.len());
    let out: Vec<u8> = reply.drain(..n).collect();
    Some(out)
}

/// Write a transfer's result back, unlink it, and push it onto the done queue.
fn retire(board: &mut Gandalf, ed: &Ed, td: &Td, stalled: bool) {
    let cc = if stalled { CC_STALL } else { CC_NO_ERROR };
    let dword0 = (td.dword0 & 0x0FFF_FFFF) | (cc << 28);
    board.write32(td.at, dword0);
    // A completed transfer reports no bytes left over.
    board.write32(td.at + 4, 0);

    // The done queue is a stack: point this at the current head and become
    // the head. Reversing it reverses the driver's idea of what finished.
    let done = board.soc.ohci.done_head;
    board.write32(td.at + 8, done);
    board.soc.ohci.done_head = td.at;

    // Unlink: the endpoint's head becomes the next transfer, keeping the
    // toggle carry, and taking the halt if this one stalled.
    let carry = ed.head_word & 2;
    let halt = if stalled { 1 } else { 0 };
    board.write32(ed.at + 8, td.next | carry | halt);
}

/// Give the driver the done queue, and tell it there is one.
fn hand_over_done_queue(board: &mut Gandalf) {
    let done = board.soc.ohci.done_head;
    if done == 0 {
        return;
    }
    let hcca = board.soc.ohci.hcca;
    if hcca != 0 {
        board.write32(hcca + HCCA_DONE_HEAD, done);
    }
    board.soc.ohci.done_head = 0;
    let soc = &mut board.soc;
    soc.ohci.raise_from_board(INT_WRITEBACK_DONE, &mut soc.intc);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page rule, which is the one that corrupts memory rather than
    /// failing: a buffer whose end is in another page is two runs, not one.
    #[test]
    fn a_buffer_that_straddles_a_page_is_two_runs() {
        let td = Td {
            at: 0,
            dword0: 0,
            buffer: 0xA000_0FFE,
            next: 0,
            end: 0xA000_1001,
        };
        assert_eq!(
            td.addresses(),
            vec![0xA000_0FFE, 0xA000_0FFF, 0xA000_1000, 0xA000_1001]
        );
    }

    #[test]
    fn a_buffer_within_one_page_is_contiguous() {
        let td = Td {
            at: 0,
            dword0: 0,
            buffer: 0xA000_0010,
            next: 0,
            end: 0xA000_0013,
        };
        assert_eq!(td.addresses().len(), 4);
    }

    /// A status stage has no buffer at all, and must not be read as one byte
    /// at address zero.
    #[test]
    fn no_buffer_means_no_bytes() {
        let td = Td {
            at: 0,
            dword0: 0,
            buffer: 0,
            next: 0,
            end: 0,
        };
        assert!(td.addresses().is_empty());
    }

    /// Skip and halt are the two ways an endpoint says "not now", and both
    /// have to be honoured or a halted endpoint spins for ever.
    #[test]
    fn skip_and_halt_are_read_from_the_right_bits() {
        let ed = Ed {
            at: 0,
            control: 1 << 14,
            tail: 0,
            head_word: 0,
            next: 0,
        };
        assert!(ed.skipped());
        assert!(!ed.halted());

        let ed = Ed {
            at: 0,
            control: 0,
            tail: 0,
            head_word: 0x1234_0001,
            next: 0,
        };
        assert!(ed.halted());
        assert_eq!(ed.head(), 0x1234_0000);
    }

    #[test]
    fn the_endpoint_and_address_come_out_of_the_control_word() {
        let ed = Ed {
            at: 0,
            // Address 5, endpoint 2.
            control: 5 | (2 << 7),
            tail: 0,
            head_word: 0,
            next: 0,
        };
        assert_eq!(ed.address(), 5);
        assert_eq!(ed.endpoint(), 2);
    }

    #[test]
    fn the_pid_says_which_way_a_transfer_goes() {
        let td = |dp: u32| Td {
            at: 0,
            dword0: dp << 19,
            buffer: 0,
            next: 0,
            end: 0,
        };
        assert_eq!(td(0).pid(), PID_SETUP);
        assert_eq!(td(1).pid(), PID_OUT);
        assert_eq!(td(2).pid(), PID_IN);
    }
}
