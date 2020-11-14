use ::std::collections::{HashMap, HashSet};

use crate::proto::{ClientMessage, InputDevice, ServerMessage};
use ::anyhow::Result;
use ::async_std::net::{SocketAddr, UdpSocket};

use crate::evdev;
use ::cdgram::CDGramServer;
use ::futures::future::{self, Either as FutEither};
use ::log::{debug, trace};

enum Event {
    ClientPacket(ClientMessage),
    InputEvent((u32, ::libc::input_event)),
    RemoveDevice(u32),
    NewDevice((u32, InputDevice)),
    MonitorNewDevice(evdev::Device),
    MonitorError(anyhow::Error),
}

struct ClientStates {
    synced_devices: HashSet<u32>,
    addr: SocketAddr,
}
impl ClientStates {
    fn handle_event(
        &mut self,
        event: &Event,
        devices: &HashMap<u32, InputDevice>,
    ) -> Option<ServerMessage> {
        match event {
            Event::ClientPacket(ClientMessage::Sync(devs)) => {
                let mut updates = HashMap::new();
                for (id, dev) in devs {
                    match devices.get(id) {
                        None => {
                            debug!("Telling client {} to drop {}:{}", self.addr, id, dev.name);
                            updates
                                .insert(*id, crate::proto::InputDeviceUpdate::Drop)
                                .unwrap_none()
                        }
                        Some(e) => {
                            if dev != e {
                                debug!(
                                    "Sending new state of {}:{} to client {}",
                                    id, dev.name, self.addr
                                );
                                updates
                                    .insert(
                                        *id,
                                        crate::proto::InputDeviceUpdate::Update(dev.clone()),
                                    )
                                    .unwrap_none();
                            }
                        }
                    }
                }
                // Inform the client all the devices it didn't know
                for (id, dev) in devices {
                    debug!(
                        "Sending new device {}:{} to client {}",
                        id, dev.name, self.addr
                    );
                    updates
                        .entry(*id)
                        .or_insert_with(|| crate::proto::InputDeviceUpdate::Update(dev.clone()));
                }
                self.synced_devices = devices.keys().map(|x| *x).collect();
                Some(ServerMessage::Sync(updates))
            }
            Event::RemoveDevice(dev_id) => {
                debug!("Telling client {} to drop {}", self.addr, dev_id);
                use ::std::iter::once;
                Some(ServerMessage::Sync(
                    once((*dev_id, crate::proto::InputDeviceUpdate::Drop)).collect(),
                ))
            }
            Event::NewDevice((dev_id, dev)) => {
                debug!(
                    "Sending new device {}:{} to client {}",
                    dev_id, dev.name, self.addr
                );
                use ::std::iter::once;
                Some(ServerMessage::Sync(
                    once((
                        *dev_id,
                        crate::proto::InputDeviceUpdate::Update(dev.clone()),
                    ))
                    .collect(),
                ))
            }
            Event::InputEvent((dev_id, ev)) => {
                if !self.synced_devices.contains(&dev_id) {
                    None
                } else {
                    trace!("Input from {} to client {}", dev_id, self.addr);
                    Some(ServerMessage::Event((
                        *dev_id,
                        crate::proto::InputEvent {
                            type_: ev.type_,
                            code: ev.code,
                            value: ev.value,
                        },
                    )))
                }
            }
            _ => panic!(),
        }
    }
}

fn start_device(id: u32, mut dev: evdev::Device, device_tx: ::async_std::sync::Sender<Event>) {
    ::async_std::task::spawn(async move {
        while let Ok(event) = dev.next_event().await {
            device_tx.send(Event::InputEvent((id as u32, event))).await;
        }
        device_tx.send(Event::RemoveDevice(id as u32)).await;
    });
}

fn monitor_devices(device_tx: ::async_std::sync::Sender<Event>) -> Result<!> {
    use ::anyhow::anyhow;
    use ::std::os::unix::io::AsRawFd;
    use ::udev::MonitorBuilder;
    let events = MonitorBuilder::new()?
        .match_subsystem_devtype("input", "hid")?
        .listen()?;

    // Make the socket blocking
    use ::nix::fcntl::{fcntl, FcntlArg, OFlag};
    let mut oflags = OFlag::from_bits(fcntl(events.as_raw_fd(), FcntlArg::F_GETFL)?).unwrap();
    oflags.remove(OFlag::O_NONBLOCK);
    fcntl(events.as_raw_fd(), FcntlArg::F_SETFL(oflags))?;

    for event in events {
        if let ::udev::EventType::Add = event.event_type() {
            let device_tx = device_tx.clone();
            if let Some(path) = event.devnode() {
                let path = path.to_owned();
                ::async_std::task::spawn(async move {
                    let dev = evdev::Device::open(&path).await?;
                    device_tx.send(Event::MonitorNewDevice(dev)).await;
                    Result::<_, ::anyhow::Error>::Ok(())
                });
            }
        }
    }
    Err(anyhow!("monitor stopped unexpectedly"))
}

fn get_device_state((id, dev): (u32, &evdev::Device)) -> Result<(u32, InputDevice)> {
    //dev.relative_axes_supported().
    debug!("Got evdev device {:?} rel {} cap {}", dev, dev.relative_axes_supported().bits(), dev.events_supported().bits());
    let input_id = dev.input_id();
    let state = InputDevice {
        name: dev.name().to_str()?.to_owned(),
        key_bits: dev.keys_supported().clone(),
        rel_bits: dev.relative_axes_supported().into(),
        cap: dev.events_supported().into(),
        key_vals: dev.state().key_vals.clone(),
        product: input_id.product,
        vendor: input_id.vendor,
        version: input_id.version,
    };
    Result::Ok((id as u32, state))
}

pub(crate) async fn run(global_cfg: ::config::Config, _: super::EntangledServerOpts) -> Result<!> {
    let socket = UdpSocket::bind(("0.0.0.0", 3241)).await?;
    let mut server = CDGramServer::new(
        global_cfg.public(),
        global_cfg.secret(),
        global_cfg.peers.iter().map(|p| p.public()),
        socket,
    );

    let mut active_clients = HashMap::new();
    let (device_tx, device_rx) = ::async_std::sync::channel(1024);
    // This function starts a new thread to handle the events from a device.
    // Received events will be sent through device_tx
    let mut devices: HashMap<_, _> = evdev::enumerate()
        .await?
        .into_iter()
        .enumerate()
        .map(|(id, dev)| {
            let (id, ret) = get_device_state((id as u32, &dev))?;
            debug!("Creating device {}:{}, {:?}", id, ret.name, ret);
            start_device(id as u32, dev, device_tx.clone());
            Ok((id, ret))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    let device_tx2 = device_tx.clone();
    ::std::thread::spawn(move || {
        let Err(e) = monitor_devices(device_tx2.clone());
        ::async_std::task::block_on(device_tx2.send(Event::MonitorError(e)));
    });

    loop {
        let (g, event) =
            match future::select(Box::pin(server.recv()), Box::pin(device_rx.recv())).await {
                FutEither::Left((msg, _)) => {
                    let (addr, pkt) = msg?;
                    let g = active_clients.entry(addr).or_insert_with(|| ClientStates {
                        synced_devices: HashSet::new(),
                        addr,
                    });
                    let pkt = ::bincode::deserialize(&pkt)?;
                    debug!("Got client packet {:?}", pkt);
                    (Some((g, addr)), Event::ClientPacket(pkt))
                }
                FutEither::Right((Ok(Event::RemoveDevice(id)), _)) => {
                    debug!("Device {} has died", id);
                    devices.remove(&id).unwrap();
                    (None, Event::RemoveDevice(id))
                }
                FutEither::Right((Ok(Event::MonitorNewDevice(dev)), _)) => {
                    let dev_id = devices.len();
                    let (dev_id, state) = get_device_state((dev_id as u32, &dev))?;
                    devices.insert(dev_id, state.clone()).unwrap_none();
                    start_device(dev_id, dev, device_tx.clone());
                    (None, Event::NewDevice((dev_id as u32, state)))
                }
                FutEither::Right((Ok(Event::MonitorError(e)), _)) => return Err(e.into()),
                FutEither::Right((Ok(e), _)) => (None, e),
                FutEither::Right((Err(e), _)) => return Err(e.into()),
            };

        if let Some((g, addr)) = g {
            if let Some(reply) = g.handle_event(&event, &devices) {
                server.send(addr, &::bincode::serialize(&reply)?).await?;
            }
        } else {
            for (addr, g) in active_clients.iter_mut() {
                if let Some(reply) = g.handle_event(&event, &devices) {
                    server.send(addr, &::bincode::serialize(&reply)?).await?;
                }
            }
        }
    }
}
