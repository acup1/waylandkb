//! System shortcuts must reach the compositor's normal keyboard input path.
//! A Wayland virtual keyboard is not required to run XKB actions or bindings.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

// linux/uinput.h, _IO/_IOW on the supported x86_64 and aarch64 Linux targets.
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_DEV_SETUP: libc::c_ulong = 0x405c5503;
const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;

#[derive(Debug)]
pub struct UinputKeyboard {
    file: File,
    modifiers: BTreeSet<u32>,
    ready_at: Instant,
}

impl UinputKeyboard {
    pub fn new() -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("cannot open /dev/uinput; see README.md for shortcut permissions")?;
        ioctl_value(&file, UI_SET_EVBIT, 1)?; // EV_KEY; EV_SYN is implicit.
        // Advertise a keyboard, including all the normal function/navigation
        // keys, so udev/libinput recognize it as such. No pointer capabilities.
        for code in 1..=255 {
            ioctl_value(&file, UI_SET_KEYBIT, code)?;
        }
        // SAFETY: zero is valid for all integer fields, including the name.
        let mut setup: libc::uinput_setup = unsafe { std::mem::zeroed() };
        setup.id.bustype = 0x06; // BUS_VIRTUAL
        setup.id.vendor = 0x1;
        setup.id.product = 0x1;
        for (dst, src) in setup.name.iter_mut().zip(b"waylandkb shortcuts") {
            *dst = *src as libc::c_char;
        }
        // SAFETY: fd is open; setup has the Linux UAPI layout and lives through ioctl.
        if unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_SETUP, &setup) } < 0 {
            return Err(io::Error::last_os_error()).context("UI_DEV_SETUP failed");
        }
        ioctl_value(&file, UI_DEV_CREATE, 0)?;
        Ok(Self {
            file,
            modifiers: BTreeSet::new(),
            // Create once at startup, not for every key. Allow device hotplug
            // to complete before accepting the first shortcut (never drop it).
            ready_at: Instant::now() + Duration::from_millis(500),
        })
    }

    pub fn set_modifiers(&mut self, modifiers: &[u32]) -> Result<()> {
        if !modifiers.is_empty() {
            self.wait_until_ready();
        }
        let wanted: BTreeSet<_> = modifiers.iter().copied().collect();
        for code in self
            .modifiers
            .difference(&wanted)
            .copied()
            .collect::<Vec<_>>()
        {
            write_key(&mut self.file, code, false)?;
            self.modifiers.remove(&code);
        }
        for code in wanted
            .difference(&self.modifiers)
            .copied()
            .collect::<Vec<_>>()
        {
            write_key(&mut self.file, code, true)?;
            self.modifiers.insert(code);
        }
        Ok(())
    }

    fn wait_until_ready(&self) {
        if let Some(delay) = self.ready_at.checked_duration_since(Instant::now()) {
            std::thread::sleep(delay);
        }
    }

    pub fn send(&mut self, code: u32, modifiers: &[u32]) -> Result<()> {
        self.wait_until_ready();
        self.set_modifiers(modifiers)?;
        // Always release the main key, also on a failed write. Modifiers remain
        // down until the UI releases/consumes them, e.g. for held Alt+Tab.
        let result = write_key(&mut self.file, code, true)
            .and_then(|()| write_key(&mut self.file, code, false));
        if let Err(error) = result {
            let _ = write_key(&mut self.file, code, false);
            let _ = self.set_modifiers(&[]);
            return Err(error.into());
        }
        Ok(())
    }
}

impl Drop for UinputKeyboard {
    fn drop(&mut self) {
        let _ = self.set_modifiers(&[]);
        let _ = ioctl_value(&self.file, UI_DEV_DESTROY, 0);
    }
}

fn ioctl_value(file: &File, request: libc::c_ulong, value: i32) -> Result<()> {
    // SAFETY: these ioctls take an integer (or no argument), not a pointer.
    if unsafe { libc::ioctl(file.as_raw_fd(), request, value) } < 0 {
        bail!("uinput ioctl {request:#x}: {}", io::Error::last_os_error());
    }
    Ok(())
}

fn write_key(writer: &mut impl Write, code: u32, pressed: bool) -> io::Result<()> {
    write_event(writer, 1, code as u16, i32::from(pressed))?;
    write_event(writer, 0, 0, 0) // EV_SYN / SYN_REPORT: each transition is a frame.
}

fn write_event(writer: &mut impl Write, kind: u16, code: u16, value: i32) -> io::Result<()> {
    // Serialize explicitly: no uninitialized C-struct padding is exposed.
    // input_event = two native longs (ignored timestamp), u16, u16, i32.
    let mut event = [0; std::mem::size_of::<libc::input_event>()];
    let start = 2 * std::mem::size_of::<libc::c_long>();
    event[start..start + 2].copy_from_slice(&kind.to_ne_bytes());
    event[start + 2..start + 4].copy_from_slice(&code.to_ne_bytes());
    event[start + 4..start + 8].copy_from_slice(&value.to_ne_bytes());
    // uinput requires a complete input_event in each write syscall.
    writer.write_all(&event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_transition_has_a_syn_report() {
        let mut bytes = Vec::new();
        write_key(&mut bytes, 57, true).unwrap();
        write_key(&mut bytes, 57, false).unwrap();
        let size = std::mem::size_of::<libc::input_event>();
        assert_eq!(bytes.len(), 4 * size);
        let events: Vec<_> = bytes
            .chunks_exact(size)
            .map(|event| {
                let start = 2 * std::mem::size_of::<libc::c_long>();
                (
                    u16::from_ne_bytes(event[start..start + 2].try_into().unwrap()),
                    u16::from_ne_bytes(event[start + 2..start + 4].try_into().unwrap()),
                    i32::from_ne_bytes(event[start + 4..start + 8].try_into().unwrap()),
                )
            })
            .collect();
        assert_eq!(events, [(1, 57, 1), (0, 0, 0), (1, 57, 0), (0, 0, 0)]);
    }

    #[test]
    fn write_errors_are_not_reported_as_success() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "disconnected"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(write_key(&mut Broken, 46, true).is_err());
    }
}
