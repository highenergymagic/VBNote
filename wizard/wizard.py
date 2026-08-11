"""The VBNote setup wizard.

Next, next, next, finish. It exists so that a first run is four keystrokes
rather than a command line, and it is built for somebody who cannot see it:
every control is labelled, the reading order is the tab order, and anything
that changes without being asked to -- the progress of a provision, above all
-- is announced rather than merely drawn.

The work is in `provision`, which has no user interface in it at all. This
draws a bar and gets out of the way.
"""
from __future__ import annotations

import os
import sys
import threading

import wx
import wx.adv

from . import provision
from .provision import default_emulator

TITLE = "VBNote Setup"

TERMS = """\
VBNote is an emulator of a discontinued notetaker. Before it is set up, two \
things need saying, and both are conditions of using it.

This is not a HumanWare product. It is an independent project. It is not \
affiliated with HumanWare, not endorsed by them, and not supported by them.

Do not contact HumanWare about it. They did not make it and cannot support \
it. Questions about VBNote belong with the VBNote project.

You also need the machine's firmware, which this software does not include \
and cannot obtain for you. You supply your own copy, from a machine you own.
"""

READY = """\
The machine is ready.

Its files are in {home}. Nothing else needs setting up: start VBNote and it \
will boot to the main menu.

The machine answered its own first-run questions with the default answers. \
Any of them can be changed on the machine itself, in the options menu.
"""


class TermsPage(wx.adv.WizardPageSimple):
    """What this is not, and what the user has to bring."""

    def __init__(self, parent):
        super().__init__(parent)
        box = wx.BoxSizer(wx.VERTICAL)
        heading = wx.StaticText(self, label="Before you start")
        heading.SetFont(heading.GetFont().Bold())
        box.Add(heading, 0, wx.ALL, 8)

        # Read-only rather than disabled: a disabled control is skipped by
        # some screen readers, and this is the one thing on the page worth
        # reading.
        text = wx.TextCtrl(
            self, value=TERMS,
            style=wx.TE_MULTILINE | wx.TE_READONLY | wx.TE_WORDWRAP)
        text.SetName("Terms, read only")
        box.Add(text, 1, wx.EXPAND | wx.ALL, 8)

        self.agreed = wx.CheckBox(self, label="I understand and agree")
        self.agreed.SetName("I understand and agree")
        box.Add(self.agreed, 0, wx.ALL, 8)
        self.SetSizer(box)


class FirmwarePage(wx.adv.WizardPageSimple):
    """Where the two firmware files are.

    Not in the original plan for this wizard, and it has to be: the emulator
    cannot build a machine without them, and it is the one thing here that
    only the user can supply.
    """

    #: The two files, as (label, attribute, dialog title, wildcard).
    #:
    #: The labels are what a screen reader reads out on landing in each field,
    #: so they say which file is wanted and not merely "file".
    WANTED = (
        ("Bootloader file, EBOOT.bin", "eboot",
         "Choose the bootloader, EBOOT.bin",
         "Bootloader (EBOOT.bin)|EBOOT.bin|All files|*.*"),
        ("Operating system file, NK.bin", "kernel",
         "Choose the operating system, NK.bin",
         "Operating system (NK.bin)|NK.bin|All files|*.*"),
    )

    def __init__(self, parent):
        super().__init__(parent)
        self.fields = {}
        box = wx.BoxSizer(wx.VERTICAL)
        heading = wx.StaticText(self, label="Your firmware")
        heading.SetFont(heading.GetFont().Bold())
        box.Add(heading, 0, wx.ALL, 8)
        box.Add(wx.StaticText(self, label=(
            "VBNote needs the bootloader and the operating system from a "
            "machine you own.\nThey are not included and cannot be "
            "downloaded.")), 0, wx.ALL, 8)

        grid = wx.FlexGridSizer(len(self.WANTED), 3, 8, 8)
        grid.AddGrowableCol(1, 1)
        for label, attr, title, wildcard in self.WANTED:
            # A plain field and a button rather than `wx.FilePickerCtrl`. The
            # picker is a composite: the name goes on the wrapper and the edit
            # box inside it keeps none of it, so a screen reader lands on an
            # unlabelled field and can only say "edit". Built this way, the
            # field carries its own name and the static text beside it is the
            # label a screen reader looks for.
            caption = wx.StaticText(self, label=label + ":")
            field = wx.TextCtrl(self, name=label)
            field.SetName(label)
            # Two buttons both reading "Browse" are indistinguishable in a
            # list of controls, so each says what it is for.
            button = wx.Button(self, label=f"Browse for {label}…")
            button.SetName(f"Browse for {label}")
            button.Bind(wx.EVT_BUTTON,
                        lambda _e, f=field, t=title, w=wildcard: self.browse(f, t, w))

            grid.Add(caption, 0, wx.ALIGN_CENTER_VERTICAL)
            grid.Add(field, 1, wx.EXPAND)
            grid.Add(button, 0)
            self.fields[attr] = field
            setattr(self, attr, field)
        box.Add(grid, 0, wx.EXPAND | wx.ALL, 8)
        self.SetSizer(box)

    def browse(self, field: wx.TextCtrl, title: str, wildcard: str) -> None:
        with wx.FileDialog(
            self, message=title, wildcard=wildcard,
            style=wx.FD_OPEN | wx.FD_FILE_MUST_EXIST,
        ) as dialog:
            if dialog.ShowModal() == wx.ID_OK:
                field.SetValue(dialog.GetPath())
                # Put the caret back where the user was, so the reader says
                # the field and its new contents rather than the button.
                field.SetFocus()

    def chosen(self) -> tuple[str, str]:
        return self.eboot.GetValue().strip(), self.kernel.GetValue().strip()

    def complete(self) -> bool:
        a, b = self.chosen()
        return bool(a) and bool(b) and os.path.exists(a) and os.path.exists(b)

    def missing(self) -> str:
        """Which file is not yet usable, for saying so out loud."""
        for label, attr, _title, _wildcard in self.WANTED:
            value = self.fields[attr].GetValue().strip()
            if not value:
                return f"Please choose the {label}."
            if not os.path.exists(value):
                return f"The {label} was not found at {value}."
        return ""


class ProvisionPage(wx.adv.WizardPageSimple):
    """The bar, and the machine building itself behind it."""

    def __init__(self, parent):
        super().__init__(parent)
        self.done = False
        self.failure = None
        box = wx.BoxSizer(wx.VERTICAL)
        heading = wx.StaticText(self, label="Setting up your machine")
        heading.SetFont(heading.GetFont().Bold())
        box.Add(heading, 0, wx.ALL, 8)
        box.Add(wx.StaticText(self, label=(
            "This takes a few minutes. The machine is starting for the first "
            "time and\nanswering its own setup questions.")), 0, wx.ALL, 8)

        self.gauge = wx.Gauge(self, range=1000)
        self.gauge.SetName("Setup progress")
        box.Add(self.gauge, 0, wx.EXPAND | wx.ALL, 8)

        caption = wx.StaticText(self, label="Status:")
        box.Add(caption, 0, wx.LEFT | wx.RIGHT | wx.TOP, 8)
        # A read-only field rather than a label. A label that changes is not
        # something a screen reader will read back: there is nothing to put
        # the cursor in and nothing to review. This can be read at any time,
        # line by line, and it keeps what has already happened rather than
        # replacing it -- which matters when the interesting part went by
        # while the user was listening to something else.
        self.log = wx.TextCtrl(
            self, value="",
            style=wx.TE_MULTILINE | wx.TE_READONLY | wx.TE_WORDWRAP,
            name="Setup status")
        self.log.SetName("Setup status, read only")
        box.Add(self.log, 1, wx.EXPAND | wx.ALL, 8)
        self.last = ""
        self.SetSizer(box)

    def show(self, fraction: float, message: str) -> None:
        self.gauge.SetValue(int(max(0.0, min(1.0, fraction)) * 1000))
        if message == self.last:
            return
        self.last = message
        line = f"{int(fraction * 100)}%  {message}"
        self.log.AppendText(line + "\n")


class FinishPage(wx.adv.WizardPageSimple):
    def __init__(self, parent):
        super().__init__(parent)
        box = wx.BoxSizer(wx.VERTICAL)
        heading = wx.StaticText(self, label="Ready")
        heading.SetFont(heading.GetFont().Bold())
        box.Add(heading, 0, wx.ALL, 8)
        self.text = wx.TextCtrl(
            self, value="", style=wx.TE_MULTILINE | wx.TE_READONLY | wx.TE_WORDWRAP)
        self.text.SetName("Result, read only")
        box.Add(self.text, 1, wx.EXPAND | wx.ALL, 8)
        self.SetSizer(box)

    def say(self, message: str) -> None:
        self.text.SetValue(message)


class Wizard(wx.adv.Wizard):
    def __init__(self, emulator: str, home: str):
        super().__init__(None, title=TITLE)
        self.emulator = emulator
        self.home = home
        self.worker = None

        self.terms = TermsPage(self)
        self.firmware = FirmwarePage(self)
        self.provisioning = ProvisionPage(self)
        self.finish = FinishPage(self)
        pages = [self.terms, self.firmware, self.provisioning, self.finish]
        for a, b in zip(pages, pages[1:]):
            wx.adv.WizardPageSimple.Chain(a, b)
        self.first = pages[0]

        self.Bind(wx.adv.EVT_WIZARD_PAGE_CHANGING, self.on_leaving)
        self.Bind(wx.adv.EVT_WIZARD_PAGE_CHANGED, self.on_arrived)
        self.SetPageSize(wx.Size(560, 340))

    # -- moving between pages -------------------------------------------
    def on_leaving(self, event: wx.adv.WizardEvent) -> None:
        if not event.GetDirection():
            return                      # going back is always allowed
        page = event.GetPage()
        if page is self.terms and not self.terms.agreed.GetValue():
            self.refuse("Please agree to the terms before continuing.")
            event.Veto()
        elif page is self.firmware and not self.firmware.complete():
            # Say which one, rather than that something is wrong.
            self.refuse(self.firmware.missing())
            event.Veto()
        elif page is self.provisioning and not self.provisioning.done:
            # Nothing to say: the button is disabled while it works, and this
            # is only reached if something got past that.
            event.Veto()

    def on_arrived(self, event: wx.adv.WizardEvent) -> None:
        if event.GetPage() is self.provisioning:
            self.start_provisioning()

    def refuse(self, why: str) -> None:
        wx.MessageBox(why, TITLE, wx.OK | wx.ICON_INFORMATION, self)

    # -- the work -------------------------------------------------------
    def start_provisioning(self) -> None:
        if self.worker:
            return
        eboot, kernel = self.firmware.chosen()
        maker = provision.Provisioner(self.emulator, eboot, kernel, self.home)
        self.enable_next(False)

        def report(p: provision.Progress) -> None:
            wx.CallAfter(self.provisioning.show, p.fraction, p.message)

        def work() -> None:
            try:
                maker.run(report)
            except Exception as e:                      # noqa: BLE001
                wx.CallAfter(self.provisioned, False, str(e))
            else:
                wx.CallAfter(self.provisioned, True, "")

        self.worker = threading.Thread(target=work, daemon=True)
        self.worker.start()

    def provisioned(self, ok: bool, trouble: str) -> None:
        self.worker = None
        self.provisioning.done = True
        if ok:
            self.finish.say(READY.format(home=self.home))
            self.provisioning.show(1.0, "Ready")
            self.enable_next(True)
            self.ShowPage(self.finish)
        else:
            self.provisioning.show(0.0, "Setup did not finish")
            wx.MessageBox(
                "Setting up the machine did not finish.\n\n"
                f"{trouble}\n\n"
                f"Nothing outside {self.home} was changed. You can close this "
                "and try again.",
                TITLE, wx.OK | wx.ICON_ERROR, self)

    def enable_next(self, on: bool) -> None:
        button = self.FindWindowById(wx.ID_FORWARD)
        if button:
            button.Enable(on)

    def run(self) -> bool:
        return self.RunWizard(self.first)


def main(argv=None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    home = provision.HOME
    if "--home" in argv:
        home = argv[argv.index("--home") + 1]

    app = wx.App()
    wizard = Wizard(default_emulator(), home)
    wizard.run()
    wizard.Destroy()
    app.MainLoop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
