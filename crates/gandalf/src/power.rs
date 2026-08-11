//! Battery and charger state, as `battdrvr.dll` reads it.
//!
//! The driver samples two GPIOs through a helper that is unmistakably the PXA
//! level register — GPLR0/1/2 at offsets 0x00/0x04/0x08 and GPLR3 at 0x100:
//!
//! ```c
//! bool read_pin(int base, uint pin) {
//!     uint *reg = (pin >> 5) < 3 ? base + (pin >> 5) * 4 : base + 0x100;
//!     return (*reg & (1 << (pin & 31))) != 0;
//! }
//! ```
//!
//! **GPIO 14 — AC adapter, active low.** `FUN_02201cac` is just
//! `return !read_pin(14)`, so a low pin means the adapter is plugged in.
//! That is the usual wiring for a DC jack that grounds a sense pin.
//!
//! **GPIO 108 — charge status, sampled nine times in a row.** The charge
//! thread counts highs and lows across nine reads and collapses them:
//! all high gives state 0, all low gives state 2, and any mixture gives
//! state 1. A line that has to be sampled repeatedly to see whether it is
//! *toggling* is a charge indicator, the same signal that blinks an LED.
//!
//! Which steady level means "charged" and which means "fault" is **not
//! proven** — the driver only compares the collapsed state against its
//! previous value. Steady high is the default here because a floating,
//! pulled-up sense line is the quiet case, and because it is the state that
//! keeps the machine running.

use pxa270::gpio::Gpio;
use pxa270::intc::Intc;

/// The power switch, active low: the pin is pulled low in the off position.
///
/// `PwrButton.dll` waits on the GPIO 0 interrupt, reads the pin, and acts when
/// it reads **low**. It is a **switch** -- two positions, and it stays where
/// it is put -- but what it is wired to is the sleep line rather than the
/// power. The machine is never off in the sense of having no power: it goes on
/// running alarms and recordings while the switch says off, which is a
/// suspended handheld of the period rather than a dead one.
///
/// So the pin is a level, held for as long as the switch is there, and what
/// the machine acts on is the *transition* across it -- down to sleep, up to
/// wake. The OAL arms this pin for a falling edge while running, and names it
/// in `PWER` as a wake source for while it is asleep, which is the two halves
/// of exactly that. Measured at the moment it sleeps: `PWER 0x80004001`, and
/// `PRER` and `PFER` both `0x00004001`, so either edge will wake it.
///
/// Every emulated GPIO input starts at zero, so before this was driven the
/// guest saw the switch in the off position from the moment the driver
/// loaded, and dutifully shut the machine off a few seconds into boot.
/// Constructing the board is the equivalent of flicking it on.
pub const GPIO_POWER_SWITCH: u32 = 0;

/// The mains adapter, active low: driven low while it is connected.
pub const GPIO_AC_PRESENT: u32 = 14;
/// Charge indicator, read as steady-high, steady-low or toggling.
pub const GPIO_CHARGE_STATUS: u32 = 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeSignal {
    /// Steady high, which the driver collapses to state 0.
    SteadyHigh,
    /// Steady low, which the driver collapses to state 2.
    SteadyLow,
    /// Alternating, which the driver collapses to state 1.
    Toggling,
}

#[derive(Debug, Clone, Copy)]
pub struct PowerState {
    pub ac_present: bool,
    /// Position of the power switch. False is off, and the guest will
    /// shut down when it notices.
    pub power_switch_on: bool,
    pub charge: ChargeSignal,
    /// Flips on each sample while `charge` is `Toggling`.
    toggle: bool,
}

impl Default for PowerState {
    fn default() -> Self {
        // An emulated machine is always on mains. Nothing here models a
        // battery discharging, and a guest that believes it has neither
        // adapter nor charge will shut itself down.
        PowerState {
            ac_present: true,
            power_switch_on: true,
            charge: ChargeSignal::SteadyHigh,
            toggle: false,
        }
    }
}

impl PowerState {
    /// Drive the sense pins to match this state. Called whenever the state
    /// changes and once per charge sample so a toggling line actually
    /// toggles.
    pub fn drive(&mut self, gpio: &mut Gpio, intc: &mut Intc) {
        gpio.set_input(GPIO_POWER_SWITCH, self.power_switch_on, intc);
        gpio.set_input(GPIO_AC_PRESENT, !self.ac_present, intc);
        let level = match self.charge {
            ChargeSignal::SteadyHigh => true,
            ChargeSignal::SteadyLow => false,
            ChargeSignal::Toggling => {
                self.toggle = !self.toggle;
                self.toggle
            }
        };
        gpio.set_input(GPIO_CHARGE_STATUS, level, intc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduce the driver's own collapse of nine samples.
    fn sample_charge(state: &mut PowerState, gpio: &mut Gpio, intc: &mut Intc) -> u32 {
        let (mut high, mut low) = (0, 0);
        for _ in 0..9 {
            state.drive(gpio, intc);
            let bank = (GPIO_CHARGE_STATUS / 32) as usize;
            let bit = 1u32 << (GPIO_CHARGE_STATUS % 32);
            if gpio.level(bank) & bit != 0 {
                high += 1;
            } else {
                low += 1;
            }
        }
        match (high, low) {
            (_, 0) => 0,
            (0, _) => 2,
            _ => 1,
        }
    }

    #[test]
    fn the_power_switch_starts_in_the_on_position() {
        let (mut gpio, mut intc) = (Gpio::default(), Intc::default());
        let mut p = PowerState::default();
        p.drive(&mut gpio, &mut intc);
        let bit = 1u32 << GPIO_POWER_SWITCH;
        assert_ne!(gpio.level(0) & bit, 0, "switched on reads high");

        p.power_switch_on = false;
        p.drive(&mut gpio, &mut intc);
        assert_eq!(gpio.level(0) & bit, 0, "switched off pulls the pin low");
    }

    #[test]
    fn the_adapter_pin_is_active_low() {
        let (mut gpio, mut intc) = (Gpio::default(), Intc::default());
        let mut p = PowerState::default();
        p.drive(&mut gpio, &mut intc);
        let bit = 1u32 << GPIO_AC_PRESENT;
        assert_eq!(gpio.level(0) & bit, 0, "adapter connected drives the pin low");

        p.ac_present = false;
        p.drive(&mut gpio, &mut intc);
        assert_ne!(gpio.level(0) & bit, 0, "adapter removed lets it float high");
    }

    #[test]
    fn a_steady_charge_line_collapses_to_a_steady_state() {
        let (mut gpio, mut intc) = (Gpio::default(), Intc::default());
        let mut p = PowerState { charge: ChargeSignal::SteadyHigh, ..Default::default() };
        assert_eq!(sample_charge(&mut p, &mut gpio, &mut intc), 0);

        let mut p = PowerState { charge: ChargeSignal::SteadyLow, ..Default::default() };
        assert_eq!(sample_charge(&mut p, &mut gpio, &mut intc), 2);
    }

    #[test]
    fn a_toggling_charge_line_is_seen_as_toggling() {
        let (mut gpio, mut intc) = (Gpio::default(), Intc::default());
        let mut p = PowerState { charge: ChargeSignal::Toggling, ..Default::default() };
        assert_eq!(sample_charge(&mut p, &mut gpio, &mut intc), 1);
    }

    #[test]
    fn charge_status_lives_in_the_fourth_gpio_bank() {
        // GPIO 108 is bank 3, which the PXA puts at register offset 0x100
        // rather than continuing the packed layout. The driver special-cases
        // it, and so must the model.
        assert_eq!(GPIO_CHARGE_STATUS / 32, 3);
    }
}
