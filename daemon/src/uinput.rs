#![allow(dead_code, non_camel_case_types)]
use ::nix::{ioctl_write_ptr, ioctl_write_int, ioctl_none, ioctl_read};
use ::libc::{c_char, c_uint};

pub const UINPUT_MAX_NAME_SIZE: usize = 80;
pub const BUS_USB: u16 = 3;

#[repr(C)]
#[derive(Clone)]
pub struct uinput_setup {
    pub id: ::libc::input_id,
    pub name: [u8; UINPUT_MAX_NAME_SIZE],
    pub ff_effects_max: u32,
}

//#[repr(C)]
//pub struct uinput_ff_upload {
//	pub request_id: uint32_t,
//	pub retval:     int32_t,
//	pub effect:     ff_effect,
//	pub old:        ff_effect,
//}
//
//#[repr(C)]
//pub struct uinput_ff_erase {
//	pub request_id: uint32_t,
//	pub retval:     int32_t,
//	pub effect_id:  uint32_t,
//}

ioctl_none!(ui_dev_create,       b'U', 1);
ioctl_none!(ui_dev_destroy,      b'U', 2);

ioctl_write_ptr!(ui_dev_setup,   b'U',   3, uinput_setup);
ioctl_write_int!(ui_set_evbit,   b'U', 100);
ioctl_write_int!(ui_set_keybit,  b'U', 101);
ioctl_write_int!(ui_set_relbit,  b'U', 102);
ioctl_write_int!(ui_set_absbit,  b'U', 103);
ioctl_write_int!(ui_set_mscbit,  b'U', 104);
ioctl_write_int!(ui_set_ledbit,  b'U', 105);
ioctl_write_int!(ui_set_sndbit,  b'U', 106);
ioctl_write_int!(ui_set_ffbit,   b'U', 107);
ioctl_write_ptr!(ui_set_phys,    b'U', 108, *const c_char);
ioctl_write_int!(ui_set_swbit,   b'U', 109);
ioctl_write_int!(ui_set_propbit, b'U', 110);

//ioctl!(readwrite ui_begin_ff_upload with b'U', 200, uinput_ff_upload);
//ioctl!(readwrite ui_end_ff_upload with b'U', 201, uinput_ff_upload);

//ioctl!(readwrite ui_begin_ff_erase with b'U', 200, uinput_ff_erase);
//ioctl!(readwrite ui_end_ff_erase with b'U', 201, uinput_ff_erase);

ioctl_read!(ui_get_version,      b'U',  45, c_uint);

#[cfg(test)]
#[test]
fn test_version() {
    use ::nix::{fcntl::OFlag, sys::stat::Mode};
    let fd = ::nix::fcntl::open("/dev/uinput", OFlag::O_WRONLY | OFlag::O_NONBLOCK, Mode::empty()).unwrap();

    let mut version: c_uint = 0;
    unsafe { ui_get_version(fd, &mut version) }.unwrap();
    eprintln!("{}", version);
}
