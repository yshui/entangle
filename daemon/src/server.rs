use ::std::collections::{HashMap, HashSet};

use crate::proto::{ClientMessage, InputDevice, ServerMessage};
use ::anyhow::Result;
use ::async_std::net::{SocketAddr, UdpSocket};

use crate::evdev;
use ::async_std::sync::{Arc, Mutex};
use ::cdgram::CDGramServer;
use ::log::{debug, info, trace};

#[derive(Clone, Debug)]
enum Event {
    ClientPacket(ClientMessage),
    InputEvent((u32, ::libc::input_event)),
    RemoveDevice(u32),
    NewDevice((u32, InputDevice)),
}

#[derive(Debug)]
enum ControlEvent {
    Event(Event),
    MonitorNewDevice(evdev::Device),
    MonitorError(anyhow::Error),
    Timeout(SocketAddr),
}

struct ClientStates {
    synced_devices: HashSet<u32>,
    addr: SocketAddr,
    timeout: Option<async_std::task::JoinHandle<()>>,
}
impl ClientStates {
    async fn handle_event(
        &mut self,
        event: &Event,
        devices: &HashMap<u32, InputDevice>,
    ) -> Option<ServerMessage> {
        if let Event::ClientPacket(_) = event {
            if let Some(h) = self.timeout.take() {
                h.cancel().await;
            }
        }
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
                self.synced_devices = devices.keys().copied().collect();
                Some(ServerMessage::Sync(updates))
            }
            Event::ClientPacket(ClientMessage::KeepAlive) => {
                debug!("Got keep alive from client {}", self.addr);
                None
            }
            Event::ClientPacket(ClientMessage::Ping) => Some(ServerMessage::Pong),
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
        }
    }
}

fn start_device(
    id: u32,
    mut dev: evdev::Device,
    device_tx: ::async_std::channel::Sender<ControlEvent>,
) {
    ::async_std::task::spawn(async move {
        debug!("Device task for dev_id {} started", id);
        while let Ok(event) = dev.next_event().await {
            debug!("Got event from dev_id {}", id);
            device_tx
                .send(ControlEvent::Event(Event::InputEvent((id as u32, event))))
                .await
                .unwrap();
        }
        device_tx
            .send(ControlEvent::Event(Event::RemoveDevice(id as u32)))
            .await
            .unwrap();
    });
}

fn monitor_devices(device_tx: ::async_std::channel::Sender<ControlEvent>) -> Result<!> {
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
                    device_tx
                        .send(ControlEvent::MonitorNewDevice(dev))
                        .await
                        .unwrap();
                    Result::<_, ::anyhow::Error>::Ok(())
                });
            }
        }
    }
    Err(anyhow!("monitor stopped unexpectedly"))
}

fn get_device_state((id, dev): (u32, &evdev::Device)) -> Result<(u32, InputDevice)> {
    //dev.relative_axes_supported().
    debug!(
        "Got evdev device {:?} rel {} cap {}",
        dev,
        dev.relative_axes_supported().bits(),
        dev.events_supported().bits()
    );
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
    let server = Arc::new(CDGramServer::new(
        global_cfg.public(),
        global_cfg.secret(),
        global_cfg.peers.iter().map(|p| p.public()),
        socket,
    ));

    let active_clients = Arc::new(Mutex::new(HashMap::new()));
    let (device_tx, device_rx) = ::async_std::channel::unbounded();
    // This function starts a new thread to handle the events from a device.
    // Received events will be sent through device_tx
    let devices: HashMap<_, _> = evdev::enumerate()
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

    // Lock order, active_clients > devices
    let devices = Arc::new(Mutex::new(devices));
    let devices2 = devices.clone();

    let device_tx2 = device_tx.clone();
    ::std::thread::spawn(move || {
        let Err(e) = monitor_devices(device_tx2.clone());
        ::async_std::task::block_on(device_tx2.send(ControlEvent::MonitorError(e))).unwrap();
    });

    let active_clients2 = active_clients.clone();
    let server2 = server.clone();
    let device_tx3 = device_tx.clone();
    let _: async_std::task::JoinHandle<Result<!>> = async_std::task::spawn(async move {
        loop {
            let msg = server.recv().await;
            let (addr, pkt) = msg?;
            let pkt = ::bincode::deserialize(&pkt)?;

            let mut active_clients = active_clients2.lock().await;
            let g = active_clients.entry(addr).or_insert_with(|| ClientStates {
                synced_devices: HashSet::new(),
                addr,
                timeout: None,
            });
            debug!("Got client packet {:?}", pkt);
            if let Some(reply) = g
                .handle_event(&Event::ClientPacket(pkt), &*devices.lock().await)
                .await
            {
                if server
                    .send(&addr, &::bincode::serialize(&reply)?)
                    .await
                    .map(|_| ())
                    .map_err(|e| {
                        info!("Error: {}", e);
                    })
                    .is_ok()
                {
                    if let Some(old_timeout) = g.timeout.take() {
                        old_timeout.cancel().await;
                    }
                    let device_tx3 = device_tx3.clone();
                    g.timeout = Some(async_std::task::spawn(async move {
                        async_std::task::sleep(std::time::Duration::from_millis(200)).await;
                        device_tx3.send(ControlEvent::Timeout(addr)).await.unwrap();
                    }))
                }
            }
        }
    });
    loop {
        let ctrl_msg = device_rx.recv().await?;
        debug!("Got control message {:?}", ctrl_msg);
        let event = match ctrl_msg {
            ControlEvent::Event(Event::RemoveDevice(id)) => {
                debug!("Device {} has died", id);
                // FIXME
                devices2.lock().await.remove(&id).unwrap();
                Event::RemoveDevice(id)
            }
            ControlEvent::MonitorNewDevice(dev) => {
                let dev_id = devices2.lock().await.len();
                let (dev_id, state) = get_device_state((dev_id as u32, &dev))?;
                devices2
                    .lock()
                    .await
                    .insert(dev_id, state.clone())
                    .unwrap_none();
                start_device(dev_id, dev, device_tx.clone());
                Event::NewDevice((dev_id as u32, state))
            }
            ControlEvent::MonitorError(e) => return Err(e),
            ControlEvent::Event(e) => e,
            ControlEvent::Timeout(addr) => {
                // Remove the timed-out task
                info!("Connection to {} has timed out, dropping it", addr);
                server2.close(addr).await.unwrap();
                let mut g = active_clients.lock().await.remove(&addr).unwrap();
                // Note: g.timeout is not necessarily the timeout task that sent us this Timeout
                // message. It could be: timeout -> new message sent -> new timeout task replaced
                // the old one -> we receive the Timeout message. In this case the new timeout
                // might still fire, so we need to cancel it.
                g.timeout.take().unwrap().cancel().await;
                continue;
            }
        };

        for (addr, g) in active_clients.lock().await.iter_mut() {
            if let Some(reply) = g.handle_event(&event, &*devices2.lock().await).await {
                if server2
                    .send(addr, &::bincode::serialize(&reply)?)
                    .await
                    .map(|_| ())
                    .map_err(|e| info!("Error: {}", e))
                    .is_ok()
                {
                    if let Some(old_timeout) = g.timeout.take() {
                        old_timeout.cancel().await;
                    }
                    let device_tx = device_tx.clone();
                    let addr = *addr;
                    g.timeout = Some(async_std::task::spawn(async move {
                        async_std::task::sleep(std::time::Duration::from_millis(200)).await;
                        device_tx.send(ControlEvent::Timeout(addr)).await.unwrap();
                    }));
                }
            }
        }
    }
}
