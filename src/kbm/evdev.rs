//! Linux `evdev` input reader and `uinput` virtual controller device.
//!
//! Compiled strictly into the `retroarch-mkb-bridge` binary so the core `save_game_manager`
//! library crate contains no `unsafe` device I/O code.

use anyhow::{Context, Result, bail};
use std::ffi::CStr;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

pub const VIRTUAL_DEVICE_NAME: &str = "RetroArch Mouse+KB Controller";
pub const VIRTUAL_VENDOR_ID: u16 = 0x2DC8;
pub const VIRTUAL_PRODUCT_ID: u16 = 0x310F;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0x00;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

pub const BTN_SOUTH: u16 = 0x130;
pub const BTN_EAST: u16 = 0x131;
pub const BTN_NORTH: u16 = 0x133;
pub const BTN_WEST: u16 = 0x134;
pub const BTN_TL: u16 = 0x136;
pub const BTN_TR: u16 = 0x137;
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;

const BUS_USB: u16 = 0x03;
const BTN_TRIGGER_HAPPY1: u16 = 0x2c0;

pub const ALL_VIRTUAL_BUTTONS: [u16; 28] = [
    BTN_SOUTH,
    BTN_EAST,
    BTN_NORTH,
    BTN_WEST,
    BTN_TL,
    BTN_TR,
    BTN_TL2,
    BTN_TR2,
    BTN_SELECT,
    BTN_START,
    BTN_THUMBL,
    BTN_THUMBR,
    BTN_TRIGGER_HAPPY1,
    BTN_TRIGGER_HAPPY1 + 1,
    BTN_TRIGGER_HAPPY1 + 2,
    BTN_TRIGGER_HAPPY1 + 3,
    BTN_TRIGGER_HAPPY1 + 4,
    BTN_TRIGGER_HAPPY1 + 5,
    BTN_TRIGGER_HAPPY1 + 6,
    BTN_TRIGGER_HAPPY1 + 7,
    BTN_TRIGGER_HAPPY1 + 8,
    BTN_TRIGGER_HAPPY1 + 9,
    BTN_TRIGGER_HAPPY1 + 10,
    BTN_TRIGGER_HAPPY1 + 11,
    BTN_TRIGGER_HAPPY1 + 12,
    BTN_TRIGGER_HAPPY1 + 13,
    BTN_TRIGGER_HAPPY1 + 14,
    BTN_TRIGGER_HAPPY1 + 15,
];

const UI_DEV_CREATE: libc::Ioctl = 0x5501;
const UI_DEV_DESTROY: libc::Ioctl = 0x5502;
const UI_DEV_SETUP: libc::Ioctl = 0x405c5503;
const UI_ABS_SETUP: libc::Ioctl = 0x401c5504;
const UI_SET_EVBIT: libc::Ioctl = 0x40045564;
const UI_SET_KEYBIT: libc::Ioctl = 0x40045565;
const UI_SET_ABSBIT: libc::Ioctl = 0x40045567;

const EVIOCGNAME_256: libc::Ioctl =
    ((2_u32 << 30) | (256_u32 << 16) | ((b'E' as u32) << 8) | 0x06) as libc::Ioctl;
const EVIOCGBIT_EV_32: libc::Ioctl =
    ((2_u32 << 30) | (32_u32 << 16) | ((b'E' as u32) << 8) | 0x20) as libc::Ioctl;
const EVIOCGBIT_KEY_96: libc::Ioctl =
    ((2_u32 << 30) | (96_u32 << 16) | ((b'E' as u32) << 8) | 0x21) as libc::Ioctl;
const EVIOCGBIT_REL_16: libc::Ioctl =
    ((2_u32 << 30) | (16_u32 << 16) | ((b'E' as u32) << 8) | 0x22) as libc::Ioctl;

const fn eviocgabs(abs: u16) -> libc::Ioctl {
    ((2_u32 << 30) | (24_u32 << 16) | ((b'E' as u32) << 8) | (0x40 + abs as u32)) as libc::Ioctl
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct InputAbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsInfo,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct InputEvent {
    pub tv_sec: libc::time_t,
    pub tv_usec: libc::suseconds_t,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

/// Virtual Linux `uinput` gamepad device.
pub struct UinputGamepad {
    fd: OwnedFd,
}

impl UinputGamepad {
    pub fn create(device_name: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")
            .context(
                "Failed to open /dev/uinput. Ensure the `uinput` kernel module is loaded \
                 and your user has ACL access (install assets/69-savevault-uinput.rules to \
                 /etc/udev/rules.d/ with `TAG+=\"uaccess\"` and reload udev).",
            )?;
        let fd = OwnedFd::from(file);
        let raw = fd.as_raw_fd();

        unsafe {
            ioctl_checked(raw, UI_SET_EVBIT, EV_KEY as libc::c_int)?;
            ioctl_checked(raw, UI_SET_EVBIT, EV_ABS as libc::c_int)?;

            for btn in ALL_VIRTUAL_BUTTONS {
                ioctl_checked(raw, UI_SET_KEYBIT, btn as libc::c_int)?;
            }

            for axis in [ABS_X, ABS_Y, ABS_RX, ABS_RY] {
                ioctl_checked(raw, UI_SET_ABSBIT, axis as libc::c_int)?;
                let abs_setup = UinputAbsSetup {
                    code: axis,
                    absinfo: InputAbsInfo {
                        value: 0,
                        minimum: -32767,
                        maximum: 32767,
                        fuzz: 0,
                        flat: 0,
                        resolution: 0,
                    },
                };
                if libc::ioctl(raw, UI_ABS_SETUP, &abs_setup) < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("UI_ABS_SETUP failed for analog stick");
                }
            }

            for hat in [ABS_HAT0X, ABS_HAT0Y] {
                ioctl_checked(raw, UI_SET_ABSBIT, hat as libc::c_int)?;
                let abs_setup = UinputAbsSetup {
                    code: hat,
                    absinfo: InputAbsInfo {
                        value: 0,
                        minimum: -1,
                        maximum: 1,
                        fuzz: 0,
                        flat: 0,
                        resolution: 0,
                    },
                };
                if libc::ioctl(raw, UI_ABS_SETUP, &abs_setup) < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("UI_ABS_SETUP failed for D-pad hat");
                }
            }

            let mut name_buf = [0u8; 80];
            let bytes = device_name.as_bytes();
            let copy_len = bytes.len().min(79);
            name_buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
            let setup = UinputSetup {
                id: InputId {
                    bustype: BUS_USB,
                    vendor: VIRTUAL_VENDOR_ID,
                    product: VIRTUAL_PRODUCT_ID,
                    version: 1,
                },
                name: name_buf,
                ff_effects_max: 0,
            };
            if libc::ioctl(raw, UI_DEV_SETUP, &setup) < 0 {
                return Err(std::io::Error::last_os_error()).context("UI_DEV_SETUP failed");
            }
            if libc::ioctl(raw, UI_DEV_CREATE) < 0 {
                return Err(std::io::Error::last_os_error()).context("UI_DEV_CREATE failed");
            }
        }

        Ok(Self { fd })
    }

    pub fn write_event(&self, type_: u16, code: u16, value: i32) -> Result<()> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        }
        let ev = InputEvent {
            tv_sec: ts.tv_sec,
            tv_usec: (ts.tv_nsec / 1000) as libc::suseconds_t,
            type_,
            code,
            value,
        };
        let ptr = &ev as *const InputEvent as *const libc::c_void;
        let sz = std::mem::size_of::<InputEvent>();
        let written = unsafe { libc::write(self.fd.as_raw_fd(), ptr, sz) };
        if written != sz as isize {
            return Err(std::io::Error::last_os_error()).context("uinput write failed");
        }
        Ok(())
    }

    pub fn syn(&self) -> Result<()> {
        self.write_event(EV_SYN, SYN_REPORT, 0)
    }
}

impl Drop for UinputGamepad {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::ioctl(self.fd.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

unsafe fn ioctl_checked(fd: RawFd, req: libc::Ioctl, val: libc::c_int) -> Result<()> {
    let rc = unsafe { libc::ioctl(fd, req, val) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).context("ioctl failed");
    }
    Ok(())
}

fn has_bit(bits: &[u8], code: u16) -> bool {
    let idx = (code / 8) as usize;
    let bit = code % 8;
    bits.get(idx).is_some_and(|b| (b & (1 << bit)) != 0)
}

pub struct EvdevSource {
    pub fd: OwnedFd,
}

pub fn scan_keyboard_and_mouse_devices() -> Vec<(PathBuf, EvdevSource)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/dev/input") else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !fname.starts_with("event") {
            continue;
        }
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let raw = file.as_raw_fd();
        let mut name_buf = [0u8; 256];
        let name_len = unsafe { libc::ioctl(raw, EVIOCGNAME_256, name_buf.as_mut_ptr()) };
        if name_len > 0 {
            let cstr = CStr::from_bytes_until_nul(&name_buf).ok();
            if cstr
                .and_then(|s| s.to_str().ok())
                .is_some_and(|name| name == VIRTUAL_DEVICE_NAME)
            {
                continue;
            }
        }
        let mut ev_bits = [0u8; 32];
        let mut rel_bits = [0u8; 16];
        let mut key_bits = [0u8; 96];
        unsafe {
            if libc::ioctl(raw, EVIOCGBIT_EV_32, ev_bits.as_mut_ptr()) < 0 {
                continue;
            }
            let _ = libc::ioctl(raw, EVIOCGBIT_REL_16, rel_bits.as_mut_ptr());
            let _ = libc::ioctl(raw, EVIOCGBIT_KEY_96, key_bits.as_mut_ptr());
        }
        let is_mouse = has_bit(&ev_bits, EV_REL)
            && has_bit(&rel_bits, REL_X)
            && has_bit(&rel_bits, REL_Y)
            && has_bit(&key_bits, 0x110); // BTN_LEFT
        let is_kbd = has_bit(&ev_bits, EV_KEY)
            && has_bit(&key_bits, 17) // KEY_W
            && has_bit(&key_bits, 30) // KEY_A
            && has_bit(&key_bits, 31) // KEY_S
            && has_bit(&key_bits, 32); // KEY_D
        if is_mouse || is_kbd {
            out.push((
                path,
                EvdevSource {
                    fd: OwnedFd::from(file),
                },
            ));
        }
    }
    out
}

/// Verifies the newly registered virtual device node appears with the expected axis ranges and keys.
pub fn verify_virtual_device_node() -> Result<PathBuf> {
    let entries = fs::read_dir("/dev/input")
        .context("Failed to inspect /dev/input for virtual gamepad verification")?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"))
        {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).open(&path) else {
            continue;
        };
        let raw = file.as_raw_fd();
        let mut name_buf = [0u8; 256];
        let len = unsafe { libc::ioctl(raw, EVIOCGNAME_256, name_buf.as_mut_ptr()) };
        if len <= 0 {
            continue;
        }
        let Ok(cstr) = CStr::from_bytes_until_nul(&name_buf) else {
            continue;
        };
        if cstr.to_str().ok() != Some(VIRTUAL_DEVICE_NAME) {
            continue;
        }
        for axis in [ABS_X, ABS_Y, ABS_RX, ABS_RY] {
            let mut info = InputAbsInfo::default();
            let rc = unsafe { libc::ioctl(raw, eviocgabs(axis), &mut info) };
            if rc < 0 {
                bail!("EVIOCGABS failed for axis {axis}");
            }
            if info.minimum != -32767 || info.maximum != 32767 {
                bail!(
                    "unexpected absinfo for axis {axis}: min={} max={}",
                    info.minimum,
                    info.maximum
                );
            }
        }
        for hat in [ABS_HAT0X, ABS_HAT0Y] {
            let mut info = InputAbsInfo::default();
            let rc = unsafe { libc::ioctl(raw, eviocgabs(hat), &mut info) };
            if rc < 0 {
                bail!("EVIOCGABS failed for hat {hat}");
            }
            if info.minimum != -1 || info.maximum != 1 {
                bail!(
                    "unexpected absinfo for hat {hat}: min={} max={}",
                    info.minimum,
                    info.maximum
                );
            }
        }
        let mut key_bits = [0u8; 96];
        let rc = unsafe { libc::ioctl(raw, EVIOCGBIT_KEY_96, key_bits.as_mut_ptr()) };
        if rc < 0 {
            bail!("EVIOCGBIT_KEY_96 failed on virtual controller");
        }
        for btn in ALL_VIRTUAL_BUTTONS {
            if !has_bit(&key_bits, btn) {
                bail!("virtual button 0x{btn:x} missing from uinput capabilities");
            }
        }
        return Ok(path);
    }
    bail!("virtual uinput device did not appear under /dev/input/event*")
}
