use ::std::collections::HashMap;

use crate::proto::ClientMessage;
use crate::uinput;
use ::anyhow::{anyhow, Context, Result};
use ::async_std::{fs, net::UdpSocket};
use ::cdgram::CDGramClient;
use ::std::mem::ManuallyDrop;
use log::debug;

use crate::proto::ServerMessage;
struct InputDeviceState {
    state: crate::proto::InputDevice,
    dev_file: ManuallyDrop<fs::File>,
}

impl InputDeviceState {
    fn create(state: crate::proto::InputDevice) -> Result<Self> {
        let mut usetup = uinput::uinput_setup {
            id: ::libc::input_id {
                vendor: state.vendor,
                bustype: uinput::BUS_USB,
                product: state.product,
                version: state.version,
            },
            name: [0; 80],
            ff_effects_max: 0,
        };

        let name_bytes = state.name.as_bytes();
        if name_bytes.len() > usetup.name.len() {
            return Err(anyhow!("Device name is too long"));
        }
        usetup.name[0..name_bytes.len()].copy_from_slice(state.name.as_bytes());

        use ::nix::{fcntl::OFlag, sys::stat::Mode};
        let fd = ::nix::fcntl::open(
            "/dev/uinput",
            OFlag::O_WRONLY | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?;

        for c in state.cap.ones() {
            unsafe { uinput::ui_set_evbit(fd, c as _)? };
        }

        for key in state.key_bits.ones() {
            unsafe { uinput::ui_set_keybit(fd, key as _)? };
        }

        for rel in state.rel_bits.ones() {
            unsafe { uinput::ui_set_relbit(fd, rel as _)? };
        }

        unsafe {
            uinput::ui_dev_setup(fd, &usetup)?;
            uinput::ui_dev_create(fd)?;

            use ::async_std::os::unix::io::FromRawFd;
            Ok(Self {
                state,
                dev_file: ManuallyDrop::new(FromRawFd::from_raw_fd(fd)),
            })
        }
    }
}

impl Drop for InputDeviceState {
    fn drop(&mut self) {
        use ::async_std::os::unix::io::AsRawFd;
        use ::log::error;
        if let Err(e) = unsafe { uinput::ui_dev_destroy(self.dev_file.as_raw_fd()) } {
            error!("Failed to destroy device {}", e);
        }
        unsafe { ManuallyDrop::drop(&mut self.dev_file) };
    }
}

pub(crate) async fn run(
    global_cfg: ::config::Config,
    cfg: super::EntangledClientOpts,
) -> Result<!> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;

    let mut server = None;
    for peer in global_cfg.peers.iter() {
        if let Some(addr) = peer.addr {
            if addr.ip() == cfg.server {
                server = Some((peer.public(), addr));
                break;
            }
        }
    }
    let (server_pk, server_addr) = server.with_context(|| "Unpaired server".to_owned())?;
    let mut devices = HashMap::<u32, InputDeviceState>::new();

    let mut client = CDGramClient::new(global_cfg.public(), global_cfg.secret(), server_pk, socket);
    client.connect(server_addr).await?;
    client
        .send(&::bincode::serialize(&ClientMessage::Sync(HashMap::new()))?)
        .await?;
    loop {
        use ::futures::AsyncWriteExt;
        let pkt = client.recv().await?;
        let pkt: ServerMessage = ::bincode::deserialize(&pkt)?;
        match pkt {
            ServerMessage::Sync(devs) => {
                for (id, update) in devs {
                    use crate::proto::InputDeviceUpdate::*;
                    match update {
                        Update(state) => {
                            if let Some(old_device) = devices.get(&id) {
                                if old_device.state.cap != state.cap
                                    || old_device.state.key_bits != state.key_bits
                                    || old_device.state.rel_bits != state.rel_bits
                                    || old_device.state.name != state.name
                                    || old_device.state.vendor != state.vendor
                                    || old_device.state.product != state.product
                                    || old_device.state.version != state.version
                                {
                                    // Recreate the device
                                    devices.remove(&id);
                                    devices.insert(id, InputDeviceState::create(state)?);
                                } else {
                                    // Sychronize the key_vals
                                }
                            } else {
                                debug!("Got new input device {}:{:?}", id, state);
                                devices.insert(id, InputDeviceState::create(state)?);
                            }
                        }
                        Drop => {
                            devices.remove(&id);
                        }
                    }
                }
            }
            ServerMessage::Event((dev_id, ev)) => {
                debug!("Received event for {}, {:?}", dev_id, ev);
                if let Some(state) = devices.get_mut(&dev_id) {
                    let ev = ::libc::input_event {
                        time: ::libc::timeval {
                            tv_sec: 0,
                            tv_usec: 0,
                        },
                        type_: ev.type_,
                        code: ev.code,
                        value: ev.value,
                    };
                    debug!("Writing device event {:?}", ev);
                    let data = unsafe {
                        ::std::slice::from_raw_parts(
                            &ev as *const _ as *const _,
                            ::std::mem::size_of_val(&ev),
                        )
                    };
                    state.dev_file.write(data).await?;
                    if (ev.type_ as u32) == crate::evdev::Types::SYNCHRONIZATION.bits().trailing_zeros() {
                        debug!("Flushing {:?}", ev);
                        state.dev_file.flush().await?;
                    }
                    debug!("Write done {:?}", ev);
                }
            }
        }
    }
}
