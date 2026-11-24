//! Userspace NAT for the emulated Mac's Ethernet interface.
//!
//! The guest sees a small private network (10.0.2.0/24) whose gateway is
//! 10.0.2.2. smoltcp terminates ARP, DHCP, DNS and TCP on that gateway; TCP
//! streams are relayed to the host's network stack. No host network interface
//! is touched: frames enter and leave through the Tauri commands.

use serde::Serialize;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, PacketMeta, RxToken, TxToken},
    socket::{
        tcp::{self, SocketBuffer, State as TcpState},
        udp::{self, UdpMetadata},
    },
    storage::{PacketBuffer, PacketMetadata},
    time::{Duration as SmolDuration, Instant},
    wire::{
        ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
        HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpProtocol, Ipv4Packet, TcpPacket,
        UdpPacket,
    },
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, ErrorKind, Read, Write},
    net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
        Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant as StdInstant},
};
use tauri::{AppHandle, Emitter};

const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
const SUBNET_BROADCAST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 255);
// The emulator's `ether js` driver only forwards guest frames whose
// destination starts with its generated-MAC prefix (0xb2), or that are
// broadcast. A unicast frame sent to any other address (including a
// conventional 02:... local MAC) is dropped before reaching the host.
const ADAPTER_MAC: [u8; 6] = [0xb2, 0x00, 0x00, 0x00, 0x02, 0x01];

// Ethernet II header through the largest frame the emulator's `ether js`
// driver hands to `etherRead`.
const MIN_FRAME_LEN: usize = 14;
const MAX_FRAME_LEN: usize = 1514;
const MIN_TX_FRAME_LEN: usize = 60;

const TCP_BUFFER_LEN: usize = 32 * 1024;
const MAX_CONNECTIONS: usize = 64;
const LISTEN_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(2);
const RELAY_QUEUE_DEPTH: usize = 32;
const RELAY_CHUNK_LEN: usize = 8 * 1024;
const RELAY_READ_TIMEOUT: Duration = Duration::from_millis(5);
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RELAY_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const DNS_TTL: u32 = 60;
const DHCP_LEASE_SECS: u32 = 86_400;
const DHCP_REPLY_MIN_LEN: usize = 300;

static TRACE: AtomicBool = AtomicBool::new(false);

macro_rules! trace {
    ($($arg:tt)*) => {
        if TRACE.load(Ordering::Relaxed) {
            eprintln!("ethernet: {}", format_args!($($arg)*));
        }
    };
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrameEvent {
    frame: Vec<u8>,
}

struct Nat {
    commands: Sender<Vec<u8>>,
    thread: JoinHandle<()>,
}

pub struct EthernetState {
    nat: Mutex<Option<Nat>>,
}

impl EthernetState {
    pub fn new() -> Self {
        Self {
            nat: Mutex::new(None),
        }
    }

    fn close(&self) {
        let nat = self
            .nat
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(nat) = nat else {
            return;
        };
        drop(nat.commands);
        let _ = nat.thread.join();
    }
}

fn parse_mac(mac_address: &str) -> Result<[u8; 6], String> {
    let mut mac = [0u8; 6];
    let mut parts = mac_address.split(':');
    for byte in &mut mac {
        let Some(part) = parts.next() else {
            return Err(format!("invalid MAC address: {mac_address}"));
        };
        if part.is_empty()
            || part.len() > 2
            || !part.bytes().all(|character| character.is_ascii_hexdigit())
        {
            return Err(format!("invalid MAC address: {mac_address}"));
        }
        *byte = u8::from_str_radix(part, 16)
            .map_err(|_| format!("invalid MAC address: {mac_address}"))?;
    }
    if parts.next().is_some() {
        return Err(format!("invalid MAC address: {mac_address}"));
    }
    Ok(mac)
}

fn is_valid_frame_len(len: usize) -> bool {
    (MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len)
}

#[tauri::command]
pub async fn ethernet_init(
    app: AppHandle,
    mac_address: String,
    state: tauri::State<'_, EthernetState>,
) -> Result<(), String> {
    parse_mac(&mac_address)?;
    state.close();

    let (commands, receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("ethernet-nat".into())
        .spawn(move || nat_thread(app, receiver))
        .map_err(|error| format!("could not start ethernet: {error}"))?;

    *state
        .nat
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Nat { commands, thread });

    println!(
        "ethernet: {mac_address} using NAT (gateway {GATEWAY_IP}, adapter {})",
        EthernetAddress(ADAPTER_MAC)
    );
    Ok(())
}

#[tauri::command]
pub async fn ethernet_send(
    _destination: String,
    frame: Vec<u8>,
    state: tauri::State<'_, EthernetState>,
) -> Result<(), String> {
    if !is_valid_frame_len(frame.len()) {
        return Err(format!("invalid ethernet frame length: {}", frame.len()));
    }
    let sender = state
        .nat
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|nat| nat.commands.clone());
    if let Some(sender) = sender {
        let _ = sender.send(frame);
    }
    Ok(())
}

fn nat_thread(app: AppHandle, commands: Receiver<Vec<u8>>) {
    let mut core = NatCore::new();
    loop {
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(frame) => core.push_frame(frame),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(frame) = commands.try_recv() {
            core.push_frame(frame);
        }
        let frames = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.tick())) {
            Ok(frames) => frames,
            Err(_) => {
                eprintln!("ethernet: NAT thread panicked; ethernet is stopped");
                return;
            }
        };
        for frame in frames {
            let _ = app.emit_to("main", "ethernet-receive", FrameEvent { frame });
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TcpTuple {
    source: IpEndpoint,
    target: IpEndpoint,
}

struct PendingConnection {
    handle: SocketHandle,
    created: StdInstant,
}

struct Relay {
    tuple: TcpTuple,
    to_host: Option<SyncSender<Vec<u8>>>,
    pending_to_host: Vec<u8>,
    pending_to_guest: VecDeque<u8>,
    host_eof: bool,
    host_failed: bool,
}

enum RelayEvent {
    Data(SocketHandle, Vec<u8>),
    Closed(SocketHandle),
    Failed(SocketHandle),
}

struct Datagram {
    endpoint: IpEndpoint,
    local_address: Option<IpAddress>,
    payload: Vec<u8>,
}

struct NatCore {
    iface: Interface,
    device: NatDevice,
    sockets: SocketSet<'static>,
    dhcp_handle: SocketHandle,
    dns_handle: SocketHandle,
    pending: HashMap<TcpTuple, PendingConnection>,
    active: HashMap<TcpTuple, SocketHandle>,
    relays: HashMap<SocketHandle, Relay>,
    relay_events: Receiver<RelayEvent>,
    relay_event_tx: SyncSender<RelayEvent>,
    dns_replies: Receiver<Datagram>,
    dns_reply_tx: Sender<Datagram>,
    datagrams: VecDeque<(SocketHandle, Datagram)>,
    removals: Vec<SocketHandle>,
    allow_loopback: bool,
    started: StdInstant,
}

impl NatCore {
    fn new() -> Self {
        Self::with_loopback(false)
    }

    fn with_loopback(allow_loopback: bool) -> Self {
        TRACE.store(
            std::env::var_os("MACINTOSH_JS_ETHERNET_TRACE").is_some(),
            Ordering::Relaxed,
        );
        let mut device = NatDevice::default();
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(ADAPTER_MAC)));
        config.random_seed = random_seed();
        let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));

        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(ip(GATEWAY_IP), 24));
            // Accept ARP from, and route replies to, guests configured with a
            // different subnet or gateway.
            let _ = addrs.push(IpCidr::new(ip(Ipv4Addr::UNSPECIFIED), 0));
        });
        iface.set_any_ip(true);

        let mut sockets: SocketSet<'static> = SocketSet::new(Vec::new());
        let dhcp_handle = add_udp_socket(&mut sockets, 67);
        let dns_handle = add_udp_socket(&mut sockets, 53);
        let (dns_reply_tx, dns_replies) = mpsc::channel();
        let (relay_event_tx, relay_events) = mpsc::sync_channel(RELAY_QUEUE_DEPTH);

        Self {
            iface,
            device,
            sockets,
            dhcp_handle,
            dns_handle,
            pending: HashMap::new(),
            active: HashMap::new(),
            relays: HashMap::new(),
            relay_events,
            relay_event_tx,
            dns_replies,
            dns_reply_tx,
            datagrams: VecDeque::new(),
            removals: Vec::new(),
            allow_loopback,
            started: StdInstant::now(),
        }
    }

    fn now(&self) -> Instant {
        Instant::from_millis(self.started.elapsed().as_millis() as i64)
    }

    fn push_frame(&mut self, frame: Vec<u8>) {
        if !is_valid_frame_len(frame.len()) {
            trace!("rx dropped frame with length {}", frame.len());
            return;
        }
        if is_own_address_probe(&frame) {
            trace_frame("rx", &frame, " (own-address probe, dropped)");
            return;
        }
        trace_frame("rx", &frame, "");
        self.observe_syn(&frame);
        self.device.inbound.push_back(frame);
    }

    fn observe_syn(&mut self, frame: &[u8]) {
        let Some(tuple) = tcp_syn(frame) else {
            return;
        };
        if self.pending.contains_key(&tuple) || self.active.contains_key(&tuple) {
            return;
        }
        if self.pending.len() + self.active.len() >= MAX_CONNECTIONS {
            return;
        }

        let mut socket = tcp::Socket::new(
            SocketBuffer::new(vec![0u8; TCP_BUFFER_LEN]),
            SocketBuffer::new(vec![0u8; TCP_BUFFER_LEN]),
        );
        if socket.listen(tuple.target).is_err() {
            return;
        }
        socket.set_nagle_enabled(false);
        socket.set_ack_delay(None);
        socket.set_keep_alive(Some(SmolDuration::from_secs(120)));

        let handle = self.sockets.add(socket);
        self.pending.insert(
            tuple,
            PendingConnection {
                handle,
                created: StdInstant::now(),
            },
        );
    }

    fn tick(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        self.poll_iface(&mut frames);
        self.service_udp();
        self.promote_pending();
        self.service_relays();
        self.poll_iface(&mut frames);

        // Aborted sockets emit their RST during the poll above, so they can
        // only be removed afterwards.
        for handle in std::mem::take(&mut self.removals) {
            let _ = self.sockets.remove(handle);
        }

        frames
    }

    fn poll_iface(&mut self, frames: &mut Vec<Vec<u8>>) {
        let now = self.now();
        let _ = self.iface.poll(now, &mut self.device, &mut self.sockets);
        if TRACE.load(Ordering::Relaxed) {
            for frame in &self.device.outbound {
                trace_frame("tx", frame, "");
            }
        }
        frames.append(&mut self.device.outbound);
    }

    fn service_udp(&mut self) {
        loop {
            let message = {
                let socket = self.sockets.get_mut::<udp::Socket>(self.dhcp_handle);
                match socket.recv() {
                    Ok((payload, meta)) => Some((payload.to_vec(), meta)),
                    Err(_) => None,
                }
            };
            let Some((payload, _meta)) = message else {
                break;
            };
            if let Some(kind) =
                find_dhcp_option(&payload, 53).and_then(|value| value.first().copied())
            {
                trace!("dhcp request type {kind}");
            }
            if let Some((destination, reply)) = dhcp_reply(&payload) {
                trace!("dhcp reply to {destination}");
                self.datagrams.push_back((
                    self.dhcp_handle,
                    Datagram {
                        endpoint: ip_endpoint(destination, 68),
                        local_address: Some(ip(GATEWAY_IP)),
                        payload: reply,
                    },
                ));
            }
        }

        loop {
            let message = {
                let socket = self.sockets.get_mut::<udp::Socket>(self.dns_handle);
                match socket.recv() {
                    Ok((payload, meta)) => Some((payload.to_vec(), meta)),
                    Err(_) => None,
                }
            };
            let Some((payload, meta)) = message else {
                break;
            };
            if let Some(question) = dns_question(&payload) {
                trace!("dns query {} type {}", question.name, question.qtype);
            }
            let replies = self.dns_reply_tx.clone();
            let _ = thread::Builder::new()
                .name("ethernet-dns".into())
                .spawn(move || {
                    if let Some(response) = answer_dns(&payload) {
                        if response.len() >= 12 {
                            let answers = u16::from_be_bytes([response[6], response[7]]);
                            trace!(
                                "dns response answers={} rcode={}",
                                answers,
                                response[3] & 0x0f
                            );
                        }
                        let _ = replies.send(Datagram {
                            endpoint: meta.endpoint,
                            local_address: meta.local_address,
                            payload: response,
                        });
                    }
                });
        }

        while let Ok(reply) = self.dns_replies.try_recv() {
            self.datagrams.push_back((self.dns_handle, reply));
        }

        while let Some((handle, datagram)) = self.datagrams.front() {
            let handle = *handle;
            let sent = {
                let socket = self.sockets.get_mut::<udp::Socket>(handle);
                if !socket.can_send() {
                    false
                } else {
                    let meta = UdpMetadata {
                        endpoint: datagram.endpoint,
                        local_address: datagram.local_address,
                        meta: PacketMeta::default(),
                    };
                    socket.send_slice(&datagram.payload, meta).is_ok()
                }
            };
            if !sent {
                break;
            }
            self.datagrams.pop_front();
        }
    }

    fn promote_pending(&mut self) {
        let mut ready = Vec::new();
        let mut stale = Vec::new();
        for (&tuple, pending) in self.pending.iter() {
            let state = self.sockets.get::<tcp::Socket>(pending.handle).state();
            match state {
                TcpState::Established => ready.push((tuple, pending.handle)),
                TcpState::Closed => stale.push((tuple, pending.handle)),
                TcpState::Listen | TcpState::SynReceived => {
                    if pending.created.elapsed() >= LISTEN_TIMEOUT {
                        stale.push((tuple, pending.handle));
                    }
                }
                _ => stale.push((tuple, pending.handle)),
            }
        }

        for (tuple, handle) in stale {
            self.pending.remove(&tuple);
            self.abort_socket(handle);
        }

        for (tuple, handle) in ready {
            self.pending.remove(&tuple);
            self.start_relay(tuple, handle);
        }
    }

    fn start_relay(&mut self, tuple: TcpTuple, handle: SocketHandle) {
        trace!("relay {} -> {}", tuple.source, tuple.target);
        let target = {
            let socket = self.sockets.get::<tcp::Socket>(handle);
            socket
                .local_endpoint()
                .and_then(|endpoint| relay_target(endpoint, self.allow_loopback))
        };
        let Some(target) = target else {
            self.abort_socket(handle);
            return;
        };

        let (to_host, from_guest) = mpsc::sync_channel(RELAY_QUEUE_DEPTH);
        let events = self.relay_event_tx.clone();
        let spawned = thread::Builder::new()
            .name("ethernet-relay".into())
            .spawn(move || relay_connection(target, from_guest, events, handle));

        match spawned {
            Ok(_) => {
                self.relays.insert(
                    handle,
                    Relay {
                        tuple,
                        to_host: Some(to_host),
                        pending_to_host: Vec::new(),
                        pending_to_guest: VecDeque::new(),
                        host_eof: false,
                        host_failed: false,
                    },
                );
                self.active.insert(tuple, handle);
            }
            Err(error) => {
                eprintln!("ethernet: could not start relay: {error}");
                self.abort_socket(handle);
            }
        }
    }

    fn abort_socket(&mut self, handle: SocketHandle) {
        {
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            socket.abort();
        }
        self.schedule_removal(handle);
    }

    fn schedule_removal(&mut self, handle: SocketHandle) {
        if !self.removals.contains(&handle) {
            self.removals.push(handle);
        }
    }

    fn service_relays(&mut self) {
        while let Ok(event) = self.relay_events.try_recv() {
            match event {
                RelayEvent::Data(handle, bytes) => {
                    if let Some(relay) = self.relays.get_mut(&handle) {
                        relay.pending_to_guest.extend(bytes);
                    }
                }
                RelayEvent::Closed(handle) => {
                    if let Some(relay) = self.relays.get_mut(&handle) {
                        relay.host_eof = true;
                    }
                }
                RelayEvent::Failed(handle) => {
                    if let Some(relay) = self.relays.get_mut(&handle) {
                        relay.host_failed = true;
                    }
                }
            }
        }

        let mut finished = Vec::new();
        for (&handle, relay) in self.relays.iter_mut() {
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            if relay.host_failed {
                socket.abort();
                finished.push(handle);
                continue;
            }

            if relay.to_host.is_some() {
                while relay.pending_to_host.is_empty() && socket.can_recv() {
                    let mut buffer = [0u8; RELAY_CHUNK_LEN];
                    match socket.recv_slice(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => relay.pending_to_host.extend_from_slice(&buffer[..count]),
                        Err(_) => break,
                    }
                }

                if !relay.pending_to_host.is_empty() {
                    let chunk = std::mem::take(&mut relay.pending_to_host);
                    if let Some(sender) = relay.to_host.as_ref() {
                        match sender.try_send(chunk) {
                            Ok(()) => {}
                            Err(TrySendError::Full(chunk)) => relay.pending_to_host = chunk,
                            Err(TrySendError::Disconnected(_)) => relay.to_host = None,
                        }
                    }
                }

                if socket.state() == TcpState::CloseWait && relay.pending_to_host.is_empty() {
                    relay.to_host = None;
                }
            }

            while !relay.pending_to_guest.is_empty() && socket.can_send() {
                relay.pending_to_guest.make_contiguous();
                match socket.send_slice(relay.pending_to_guest.as_slices().0) {
                    Ok(0) => break,
                    Ok(count) => {
                        relay.pending_to_guest.drain(..count);
                    }
                    Err(_) => break,
                }
            }

            if relay.host_eof
                && relay.pending_to_guest.is_empty()
                && matches!(socket.state(), TcpState::Established | TcpState::CloseWait)
            {
                socket.close();
            }

            if matches!(socket.state(), TcpState::Closed | TcpState::TimeWait) {
                finished.push(handle);
            }
        }

        for handle in finished {
            if let Some(relay) = self.relays.remove(&handle) {
                self.active.remove(&relay.tuple);
            }
            self.schedule_removal(handle);
        }
    }
}

fn add_udp_socket(sockets: &mut SocketSet<'static>, port: u16) -> SocketHandle {
    let mut socket = udp::Socket::new(
        PacketBuffer::new(vec![PacketMetadata::EMPTY; 8], vec![0u8; 4096]),
        PacketBuffer::new(vec![PacketMetadata::EMPTY; 8], vec![0u8; 4096]),
    );
    socket.bind(port).expect("NAT ports are valid");
    sockets.add(socket)
}

fn ip(address: Ipv4Addr) -> IpAddress {
    IpAddress::v4(
        address.octets()[0],
        address.octets()[1],
        address.octets()[2],
        address.octets()[3],
    )
}

fn ip_endpoint(address: Ipv4Addr, port: u16) -> IpEndpoint {
    IpEndpoint::new(ip(address), port)
}

fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0x5eed_1234)
        | 1
}

// `any_ip` makes smoltcp answer ARP for every target, which is what lets a
// guest with an arbitrary gateway or netmask reach us. It would also answer
// the guest's own probes for its new address, which Open Transport reports as
// a duplicate-address conflict, so those requests never reach smoltcp.
fn is_own_address_probe(frame: &[u8]) -> bool {
    let ethernet = match EthernetFrame::new_checked(frame) {
        Ok(ethernet) => ethernet,
        Err(_) => return false,
    };
    if ethernet.ethertype() != EthernetProtocol::Arp {
        return false;
    }
    let packet = match ArpPacket::new_checked(ethernet.payload()) {
        Ok(packet) => packet,
        Err(_) => return false,
    };
    let reply = match ArpRepr::parse(&packet) {
        Ok(reply) => reply,
        Err(_) => return false,
    };

    match reply {
        ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Request,
            source_protocol_addr,
            target_protocol_addr,
            ..
        } => target_protocol_addr == GUEST_IP || source_protocol_addr == target_protocol_addr,
        _ => false,
    }
}

fn tcp_syn(frame: &[u8]) -> Option<TcpTuple> {
    let ethernet = EthernetFrame::new_checked(frame).ok()?;
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let packet = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
    if packet.next_header() != IpProtocol::Tcp {
        return None;
    }
    let tcp = TcpPacket::new_checked(packet.payload()).ok()?;
    if !tcp.syn() || tcp.ack() {
        return None;
    }

    Some(TcpTuple {
        source: IpEndpoint::new(ip(packet.src_addr()), tcp.src_port()),
        target: IpEndpoint::new(ip(packet.dst_addr()), tcp.dst_port()),
    })
}

fn relay_target(endpoint: IpEndpoint, allow_loopback: bool) -> Option<SocketAddrV4> {
    match endpoint.addr {
        IpAddress::Ipv4(address) => {
            let allowed = allow_loopback && address.is_loopback();
            if is_blocked_destination(address) && !allowed {
                None
            } else {
                Some(SocketAddrV4::new(address, endpoint.port))
            }
        }
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

fn is_blocked_destination(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_unspecified()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || octets[0] == 0
        || octets[0] >= 240
        || (octets[0] == 10 && octets[1] == 0 && octets[2] == 2)
}

fn trace_frame(direction: &str, frame: &[u8], note: &str) {
    if !TRACE.load(Ordering::Relaxed) {
        return;
    }
    match frame_summary(frame) {
        Some(summary) => eprintln!("ethernet: {direction} {summary}{note}"),
        None => eprintln!("ethernet: {direction} frame len={}{note}", frame.len()),
    }
}

fn frame_summary(frame: &[u8]) -> Option<String> {
    let ethernet = EthernetFrame::new_checked(frame).ok()?;
    let route = format!("{} -> {}", ethernet.src_addr(), ethernet.dst_addr());
    match ethernet.ethertype() {
        EthernetProtocol::Arp => {
            let packet = ArpPacket::new_checked(ethernet.payload()).ok()?;
            match ArpRepr::parse(&packet).ok()? {
                ArpRepr::EthernetIpv4 {
                    operation,
                    source_protocol_addr,
                    target_protocol_addr,
                    ..
                } => {
                    let operation = match operation {
                        ArpOperation::Request => "request",
                        ArpOperation::Reply => "reply",
                        _ => "operation",
                    };
                    Some(format!(
                        "{route} arp {operation} {source_protocol_addr} -> {target_protocol_addr}"
                    ))
                }
                _ => Some(format!("{route} arp")),
            }
        }
        EthernetProtocol::Ipv4 => {
            let packet = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
            match packet.next_header() {
                IpProtocol::Tcp => {
                    let tcp = TcpPacket::new_checked(packet.payload()).ok()?;
                    Some(format!(
                        "{route} tcp {}:{} -> {}:{} {} len={}",
                        packet.src_addr(),
                        tcp.src_port(),
                        packet.dst_addr(),
                        tcp.dst_port(),
                        tcp_flags(&tcp),
                        tcp.segment_len()
                    ))
                }
                IpProtocol::Udp => {
                    let udp = UdpPacket::new_checked(packet.payload()).ok()?;
                    Some(format!(
                        "{route} udp {}:{} -> {}:{} len={}",
                        packet.src_addr(),
                        udp.src_port(),
                        packet.dst_addr(),
                        udp.dst_port(),
                        udp.payload().len()
                    ))
                }
                protocol => Some(format!(
                    "{route} ipv4 {protocol} {} -> {} len={}",
                    packet.src_addr(),
                    packet.dst_addr(),
                    packet.payload().len()
                )),
            }
        }
        protocol => Some(format!("{route} {protocol} len={}", frame.len())),
    }
}

fn tcp_flags<T: AsRef<[u8]>>(packet: &TcpPacket<T>) -> String {
    let mut flags = String::new();
    if packet.syn() {
        flags.push('S');
    }
    if packet.ack() {
        flags.push('A');
    }
    if packet.fin() {
        flags.push('F');
    }
    if packet.rst() {
        flags.push('R');
    }
    if packet.psh() {
        flags.push('P');
    }
    if flags.is_empty() {
        flags.push('-');
    }
    flags
}

fn relay_connection(
    target: SocketAddrV4,
    from_guest: Receiver<Vec<u8>>,
    events: SyncSender<RelayEvent>,
    handle: SocketHandle,
) {
    let address = SocketAddr::V4(target);
    let mut stream = match TcpStream::connect_timeout(&address, RELAY_CONNECT_TIMEOUT) {
        Ok(stream) => {
            trace!("host connected {address}");
            stream
        }
        Err(error) => {
            trace!("host connect failed {address}: {error}");
            let _ = events.send(RelayEvent::Failed(handle));
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(RELAY_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RELAY_WRITE_TIMEOUT));

    let mut buffer = vec![0u8; RELAY_CHUNK_LEN];
    let mut guest_open = true;
    loop {
        if guest_open {
            loop {
                match from_guest.try_recv() {
                    Ok(bytes) => {
                        if write_all(&mut stream, &bytes).is_err() {
                            let _ = events.send(RelayEvent::Failed(handle));
                            return;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        guest_open = false;
                        let _ = stream.shutdown(Shutdown::Write);
                        break;
                    }
                }
            }
        }

        match stream.read(&mut buffer) {
            Ok(0) => {
                trace!("host closed {address}");
                let _ = events.send(RelayEvent::Closed(handle));
                return;
            }
            Ok(count) => {
                if events
                    .send(RelayEvent::Data(handle, buffer[..count].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                trace!("host read failed {address}: {error}");
                let _ = events.send(RelayEvent::Failed(handle));
                return;
            }
        }
    }
}

fn write_all(stream: &mut TcpStream, mut data: &[u8]) -> io::Result<()> {
    let deadline = StdInstant::now() + RELAY_WRITE_TIMEOUT;
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => data = &data[count..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if StdInstant::now() >= deadline {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn dhcp_reply(request: &[u8]) -> Option<(Ipv4Addr, Vec<u8>)> {
    if request.len() < 240
        || request[0] != 1
        || request[1] != 1
        || request[2] < 6
        || request[236..240] != [99, 130, 83, 99]
    {
        return None;
    }

    let message_type = find_dhcp_option(request, 53)?.first().copied()?;
    if !matches!(message_type, 1 | 3 | 8) {
        return None;
    }
    if message_type == 3 {
        if let Some(server) = find_dhcp_option(request, 54) {
            if server.len() == 4 && server[..] != GATEWAY_IP.octets()[..] {
                return None;
            }
        }
    }

    let reply_type = if message_type == 1 { 2 } else { 5 };
    let assigned = if message_type == 8 {
        Ipv4Addr::UNSPECIFIED
    } else {
        GUEST_IP
    };
    let broadcast = u16::from_be_bytes([request[10], request[11]]) & 0x8000 != 0;
    let ciaddr = Ipv4Addr::new(request[12], request[13], request[14], request[15]);
    let destination = if broadcast || ciaddr.is_unspecified() {
        Ipv4Addr::BROADCAST
    } else {
        ciaddr
    };

    let mut reply = vec![0u8; 240];
    reply[0] = 2;
    reply[1] = 1;
    reply[2] = 6;
    reply[4..8].copy_from_slice(&request[4..8]);
    reply[10..12].copy_from_slice(&request[10..12]);
    reply[12..16].copy_from_slice(&request[12..16]);
    reply[16..20].copy_from_slice(&assigned.octets());
    reply[24..28].copy_from_slice(&request[24..28]);
    reply[28..44].copy_from_slice(&request[28..44]);
    reply[236..240].copy_from_slice(&[99, 130, 83, 99]);

    push_dhcp_option(&mut reply, 53, &[reply_type]);
    push_dhcp_option(&mut reply, 54, &GATEWAY_IP.octets());

    if message_type != 8 {
        push_dhcp_option(&mut reply, 51, &DHCP_LEASE_SECS.to_be_bytes());
        push_dhcp_option(&mut reply, 1, &NETMASK.octets());
        push_dhcp_option(&mut reply, 3, &GATEWAY_IP.octets());
        push_dhcp_option(&mut reply, 6, &GATEWAY_IP.octets());
        push_dhcp_option(&mut reply, 28, &SUBNET_BROADCAST_IP.octets());
    }

    reply.push(255);
    if reply.len() < DHCP_REPLY_MIN_LEN {
        reply.resize(DHCP_REPLY_MIN_LEN, 0);
    }

    Some((destination, reply))
}

fn find_dhcp_option(payload: &[u8], code: u8) -> Option<&[u8]> {
    let mut offset = 240;
    while offset < payload.len() {
        match payload[offset] {
            0 => offset += 1,
            255 => return None,
            option => {
                let length = *payload.get(offset + 1)? as usize;
                let start = offset + 2;
                let end = start.checked_add(length)?;
                if end > payload.len() {
                    return None;
                }
                if option == code {
                    return Some(&payload[start..end]);
                }
                offset = end;
            }
        }
    }
    None
}

fn push_dhcp_option(reply: &mut Vec<u8>, code: u8, value: &[u8]) {
    reply.push(code);
    reply.push(value.len() as u8);
    reply.extend_from_slice(value);
}

struct DnsQuestion {
    name: String,
    qtype: u16,
    qclass: u16,
    end: usize,
}

fn answer_dns(query: &[u8]) -> Option<Vec<u8>> {
    let question = dns_question(query)?;
    if question.qclass != 1 || !matches!(question.qtype, 1 | 28) {
        return Some(build_dns_response(query, &question, &[], &[], 0));
    }

    let (addresses_v4, addresses_v6) = resolve_host(&question.name);
    let (answers_v4, answers_v6) = if question.qtype == 1 {
        (&addresses_v4[..], &[][..])
    } else {
        (&[][..], &addresses_v6[..])
    };
    let rcode = if addresses_v4.is_empty() && addresses_v6.is_empty() {
        3
    } else {
        0
    };
    Some(build_dns_response(
        query, &question, answers_v4, answers_v6, rcode,
    ))
}

fn dns_question(query: &[u8]) -> Option<DnsQuestion> {
    if query.len() < 12 || u16::from_be_bytes([query[4], query[5]]) != 1 {
        return None;
    }

    let mut offset = 12;
    let mut name = String::new();
    loop {
        let length = *query.get(offset)? as usize;
        if length == 0 {
            offset += 1;
            break;
        }
        if length & 0xc0 != 0 || length > 63 {
            return None;
        }
        let label = query.get(offset + 1..offset + 1 + length)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(std::str::from_utf8(label).ok()?);
        offset += length + 1;
    }

    let tail = query.get(offset..offset + 4)?;
    Some(DnsQuestion {
        name,
        qtype: u16::from_be_bytes([tail[0], tail[1]]),
        qclass: u16::from_be_bytes([tail[2], tail[3]]),
        end: offset + 4,
    })
}

fn build_dns_response(
    query: &[u8],
    question: &DnsQuestion,
    answers_v4: &[Ipv4Addr],
    answers_v6: &[Ipv6Addr],
    rcode: u8,
) -> Vec<u8> {
    let answer_count = (answers_v4.len() + answers_v6.len()).min(8);
    let mut response = Vec::with_capacity(question.end + answer_count * 28 + 16);
    response.extend_from_slice(&query[0..2]);

    let request_flags = u16::from_be_bytes([query[2], query[3]]);
    let flags = 0x8000 | (request_flags & 0x7900) | 0x0080 | u16::from(rcode & 0x0f);
    response.extend_from_slice(&flags.to_be_bytes());
    response.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
    response[6..8].copy_from_slice(&(answer_count as u16).to_be_bytes());
    response.extend_from_slice(&query[12..question.end]);

    let mut answers = 0;
    for address in answers_v4 {
        if answers == answer_count {
            break;
        }
        push_dns_answer(&mut response, 1, &address.octets());
        answers += 1;
    }
    for address in answers_v6 {
        if answers == answer_count {
            break;
        }
        push_dns_answer(&mut response, 28, &address.octets());
        answers += 1;
    }

    response
}

fn push_dns_answer(response: &mut Vec<u8>, record_type: u16, data: &[u8]) {
    response.extend_from_slice(&[0xc0, 0x0c]);
    response.extend_from_slice(&record_type.to_be_bytes());
    response.extend_from_slice(&1u16.to_be_bytes());
    response.extend_from_slice(&DNS_TTL.to_be_bytes());
    response.extend_from_slice(&(data.len() as u16).to_be_bytes());
    response.extend_from_slice(data);
}

fn resolve_host(name: &str) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    let mut addresses_v4 = Vec::new();
    let mut addresses_v6 = Vec::new();
    if let Ok(addresses) = (name, 0u16).to_socket_addrs() {
        for address in addresses {
            match address.ip() {
                std::net::IpAddr::V4(address) => {
                    if !addresses_v4.contains(&address) {
                        addresses_v4.push(address);
                    }
                }
                std::net::IpAddr::V6(address) => {
                    if !addresses_v6.contains(&address) {
                        addresses_v6.push(address);
                    }
                }
            }
        }
    }
    (addresses_v4, addresses_v6)
}

#[derive(Default)]
struct NatDevice {
    inbound: VecDeque<Vec<u8>>,
    outbound: Vec<Vec<u8>>,
}

struct RxFrame(Vec<u8>);

impl RxToken for RxFrame {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

struct TxFrame<'a> {
    outbound: &'a mut Vec<Vec<u8>>,
}

impl TxToken for TxFrame<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len.max(MIN_TX_FRAME_LEN)];
        let result = f(&mut buffer[..len]);
        self.outbound.push(buffer);
        result
    }
}

impl Device for NatDevice {
    type RxToken<'a> = RxFrame;
    type TxToken<'a> = TxFrame<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.inbound.pop_front()?;
        Some((
            RxFrame(frame),
            TxFrame {
                outbound: &mut self.outbound,
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxFrame {
            outbound: &mut self.outbound,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = MAX_FRAME_LEN;
        capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::{
        phy::ChecksumCapabilities,
        wire::{
            ArpOperation, ArpPacket, ArpRepr, EthernetRepr, Ipv4Repr, TcpControl, TcpRepr,
            TcpSeqNumber, UdpPacket, UdpRepr,
        },
    };

    const TEST_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

    #[test]
    fn parses_colon_separated_mac_addresses() {
        assert_eq!(
            parse_mac("02:00:00:00:00:01").unwrap(),
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
        );
        assert_eq!(
            parse_mac("2:0:0:0:0:1").unwrap(),
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
        );
    }

    #[test]
    fn rejects_malformed_mac_addresses() {
        for address in [
            "",
            "02:00:00:00:00",
            "02:00:00:00:00:01:02",
            "02:00:00:00:00:zz",
            "02-00-00-00-00-01",
            "02:00:00:00:00:0001",
        ] {
            assert!(parse_mac(address).is_err(), "{address} should be rejected");
        }
    }

    #[test]
    fn adapter_mac_is_forwarded_by_the_emulator() {
        // Basilisk II's ether_js.cpp only forwards unicast destinations whose
        // first octet matches GENERATED_ETHER_ADDR_PREFIX.
        assert_eq!(ADAPTER_MAC[0], 0xb2);
    }

    #[test]
    fn checks_frame_length_bounds() {
        assert!(!is_valid_frame_len(13));
        assert!(is_valid_frame_len(14));
        assert!(is_valid_frame_len(1514));
        assert!(!is_valid_frame_len(1515));
    }

    #[test]
    fn blocks_unsafe_relay_destinations() {
        assert!(is_blocked_destination(Ipv4Addr::UNSPECIFIED));
        assert!(is_blocked_destination(Ipv4Addr::LOCALHOST));
        assert!(is_blocked_destination(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(is_blocked_destination(Ipv4Addr::BROADCAST));
        assert!(is_blocked_destination(GATEWAY_IP));
        assert!(!is_blocked_destination(Ipv4Addr::new(93, 184, 216, 34)));
        assert!(!is_blocked_destination(Ipv4Addr::new(198, 18, 0, 204)));
    }

    #[test]
    fn offers_a_lease_to_dhcp_discover() {
        let request = dhcp_request(0x1234_5678, 1);
        let (destination, reply) = dhcp_reply(&request).expect("discover should be answered");

        assert_eq!(destination, Ipv4Addr::BROADCAST);
        assert_eq!(reply[0], 2);
        assert_eq!(&reply[4..8], &0x1234_5678u32.to_be_bytes());
        assert_eq!(&reply[16..20], &GUEST_IP.octets());
        assert_eq!(find_dhcp_option(&reply, 53), Some(&[2u8][..]));
        assert_eq!(find_dhcp_option(&reply, 54), Some(&GATEWAY_IP.octets()[..]));
        assert_eq!(find_dhcp_option(&reply, 1), Some(&NETMASK.octets()[..]));
        assert_eq!(find_dhcp_option(&reply, 3), Some(&GATEWAY_IP.octets()[..]));
        assert_eq!(find_dhcp_option(&reply, 6), Some(&GATEWAY_IP.octets()[..]));
    }

    #[test]
    fn acknowledges_a_lease_request() {
        let request = dhcp_request(0x0bad_f00d, 3);
        let (destination, reply) = dhcp_reply(&request).expect("request should be answered");

        assert_eq!(destination, Ipv4Addr::BROADCAST);
        assert_eq!(&reply[16..20], &GUEST_IP.octets());
        assert_eq!(find_dhcp_option(&reply, 53), Some(&[5u8][..]));
    }

    #[test]
    fn builds_dns_responses() {
        let query = dns_query("example.com", 1);
        let question = dns_question(&query).expect("question should parse");
        let answer = Ipv4Addr::new(93, 184, 216, 34);
        let response = build_dns_response(&query, &question, &[answer], &[], 0);

        assert_eq!(&response[0..2], &[0x12, 0x34]);
        assert_eq!(response[2] & 0x80, 0x80);
        assert_eq!(response[3] & 0x0f, 0);
        assert_eq!(u16::from_be_bytes([response[4], response[5]]), 1);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1);

        let answer_offset = question.end;
        assert_eq!(&response[answer_offset..answer_offset + 2], &[0xc0, 0x0c]);
        assert_eq!(
            u16::from_be_bytes([response[answer_offset + 2], response[answer_offset + 3]]),
            1
        );
        assert_eq!(
            u16::from_be_bytes([response[answer_offset + 10], response[answer_offset + 11]]),
            4
        );
        assert_eq!(
            &response[answer_offset + 12..answer_offset + 16],
            &answer.octets()
        );
    }

    #[test]
    fn creates_one_listener_per_tcp_tuple() {
        let mut core = NatCore::new();
        let syn = tcp_frame(
            TEST_MAC,
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(93, 184, 216, 34),
            49152,
            80,
        );

        core.push_frame(syn.clone());
        assert_eq!(core.pending.len(), 1);

        core.push_frame(syn);
        assert_eq!(core.pending.len(), 1);
    }

    #[test]
    fn does_not_answer_address_probes_for_the_guest() {
        let mut core = NatCore::new();
        core.push_frame(arp_request_frame(TEST_MAC, GUEST_IP, GUEST_IP));
        assert!(core.tick().is_empty());
    }

    #[test]
    fn answers_arp_for_the_gateway() {
        let mut core = NatCore::new();
        core.push_frame(arp_request_frame(TEST_MAC, GUEST_IP, GATEWAY_IP));
        let frames = core.tick();
        let reply = frames
            .iter()
            .find_map(|frame| arp_reply(frame))
            .expect("the gateway should answer ARP");

        match reply {
            ArpRepr::EthernetIpv4 {
                operation,
                source_hardware_addr,
                source_protocol_addr,
                target_protocol_addr,
                ..
            } => {
                assert_eq!(operation, ArpOperation::Reply);
                assert_eq!(source_hardware_addr, EthernetAddress(ADAPTER_MAC));
                assert_eq!(source_protocol_addr, GATEWAY_IP);
                assert_eq!(target_protocol_addr, GUEST_IP);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn completes_the_tcp_handshake_before_relaying() {
        let mut core = NatCore::new();
        // A real guest first ARPs for the gateway, which also teaches the
        // NAT the guest's hardware address.
        core.push_frame(arp_request_frame(TEST_MAC, GUEST_IP, GATEWAY_IP));
        let _ = core.tick();

        core.push_frame(tcp_repr_frame(
            TEST_MAC,
            GUEST_IP,
            Ipv4Addr::LOCALHOST,
            49152,
            80,
            TcpSeqNumber(1),
            None,
            &[],
        ));

        let frames = core.tick();
        let (source, destination, syn_ack) = frames
            .iter()
            .find_map(|frame| tcp_segment(frame))
            .expect("the NAT should answer with SYN-ACK");
        assert_eq!(source, Ipv4Addr::LOCALHOST);
        assert_eq!(destination, GUEST_IP);
        assert!(syn_ack.syn() && syn_ack.ack());
        assert_eq!(syn_ack.src_port(), 80);
        assert_eq!(syn_ack.dst_port(), 49152);

        core.push_frame(tcp_repr_frame(
            TEST_MAC,
            GUEST_IP,
            Ipv4Addr::LOCALHOST,
            49152,
            80,
            TcpSeqNumber(2),
            Some(TcpSeqNumber(syn_ack.seq_number().0 + 1)),
            &[],
        ));

        let frames = core.tick();
        assert!(
            frames
                .iter()
                .any(|frame| tcp_segment(frame).is_some_and(|segment| segment.2.rst())),
            "a blocked destination should be reset"
        );
        assert!(core.pending.is_empty());
        assert!(core.active.is_empty());
    }

    #[test]
    fn relays_a_full_connection_to_a_host_server() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind server");
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept relay");
            let mut request = [0u8; 4];
            stream.read_exact(&mut request).expect("read request");
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").expect("write response");
            stream.shutdown(Shutdown::Write).expect("close write half");
        });

        let mut core = NatCore::with_loopback(true);
        core.push_frame(arp_request_frame(TEST_MAC, GUEST_IP, GATEWAY_IP));
        let _ = core.tick();

        core.push_frame(tcp_repr_frame(
            TEST_MAC,
            GUEST_IP,
            Ipv4Addr::LOCALHOST,
            49152,
            port,
            TcpSeqNumber(1),
            None,
            &[],
        ));
        let frames = core.tick();
        let (_, _, syn_ack) = frames
            .iter()
            .find_map(|frame| tcp_segment(frame))
            .expect("the NAT should answer with SYN-ACK");
        assert!(syn_ack.syn() && syn_ack.ack());

        core.push_frame(tcp_repr_frame(
            TEST_MAC,
            GUEST_IP,
            Ipv4Addr::LOCALHOST,
            49152,
            port,
            TcpSeqNumber(2),
            Some(TcpSeqNumber(syn_ack.seq_number().0 + 1)),
            b"ping",
        ));

        let deadline = StdInstant::now() + Duration::from_secs(3);
        let mut response = Vec::new();
        while StdInstant::now() < deadline && response.len() < 4 {
            for frame in core.tick() {
                if let Some((source, destination, tcp)) = tcp_segment(&frame) {
                    if source == Ipv4Addr::LOCALHOST
                        && destination == GUEST_IP
                        && tcp.src_port() == port
                    {
                        response.extend_from_slice(tcp.payload());
                    }
                }
            }
            if response.len() < 4 {
                thread::sleep(Duration::from_millis(1));
            }
        }

        assert_eq!(response, b"pong");
        server.join().unwrap();
    }

    #[test]
    fn relays_bytes_to_and_from_the_host() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind echo server");
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept relay");
            let mut request = [0u8; 5];
            stream.read_exact(&mut request).expect("read request");
            assert_eq!(&request, b"hello");
            stream.write_all(b"world").expect("write response");
            stream.shutdown(Shutdown::Write).expect("close write half");
        });

        let (events_tx, events_rx) = mpsc::sync_channel(8);
        let (to_host_tx, to_host_rx) = mpsc::sync_channel(8);
        let relay = thread::spawn(move || {
            relay_connection(
                SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
                to_host_rx,
                events_tx,
                SocketHandle::default(),
            );
        });

        to_host_tx.send(b"hello".to_vec()).expect("send request");
        let mut received = Vec::new();
        let mut closed = false;
        while let Ok(event) = events_rx.recv_timeout(Duration::from_secs(5)) {
            match event {
                RelayEvent::Data(_, bytes) => received.extend(bytes),
                RelayEvent::Closed(_) => {
                    closed = true;
                    break;
                }
                RelayEvent::Failed(_) => panic!("relay failed"),
            }
        }

        assert_eq!(received, b"world");
        assert!(closed);
        drop(to_host_tx);
        relay.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn nat_answers_dhcp_discover() {
        let mut core = NatCore::new();
        core.push_frame(dhcp_frame(TEST_MAC, &dhcp_request(0x1020_3040, 1)));

        let frames = core.tick();
        let (destination, source_port, destination_port, payload) = frames
            .iter()
            .find_map(|frame| udp_datagram(frame).filter(|datagram| datagram.2 == 68))
            .expect("a DHCP reply should be emitted");

        assert_eq!(source_port, 67);
        assert_eq!(destination_port, 68);
        assert_eq!(destination, Ipv4Addr::BROADCAST);
        assert_eq!(find_dhcp_option(payload, 53), Some(&[2u8][..]));
        assert_eq!(payload[16..20], GUEST_IP.octets());
    }

    fn dhcp_request(xid: u32, message_type: u8) -> Vec<u8> {
        let mut payload = vec![0u8; 240];
        payload[0] = 1;
        payload[1] = 1;
        payload[2] = 6;
        payload[4..8].copy_from_slice(&xid.to_be_bytes());
        payload[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        payload[28..34].copy_from_slice(&TEST_MAC);
        payload[236..240].copy_from_slice(&[99, 130, 83, 99]);
        payload.extend_from_slice(&[53, 1, message_type]);
        payload.extend_from_slice(&[55, 3, 1, 3, 6]);
        payload.push(255);
        payload
    }

    fn dhcp_frame(mac: [u8; 6], payload: &[u8]) -> Vec<u8> {
        let ethernet = EthernetRepr {
            src_addr: EthernetAddress(mac),
            dst_addr: EthernetAddress::BROADCAST,
            ethertype: EthernetProtocol::Ipv4,
        };
        let source = Ipv4Addr::UNSPECIFIED;
        let destination = Ipv4Addr::BROADCAST;
        let ip_repr = Ipv4Repr {
            src_addr: source,
            dst_addr: destination,
            next_header: IpProtocol::Udp,
            payload_len: 8 + payload.len(),
            hop_limit: 64,
        };
        let udp = UdpRepr {
            src_port: 68,
            dst_port: 67,
        };

        let mut buffer = vec![0u8; 14 + 20 + 8 + payload.len()];
        ethernet.emit(&mut EthernetFrame::new_unchecked(&mut buffer[..]));
        ip_repr.emit(
            &mut Ipv4Packet::new_unchecked(&mut buffer[14..34]),
            &ChecksumCapabilities::default(),
        );
        udp.emit(
            &mut UdpPacket::new_unchecked(&mut buffer[34..]),
            &super::ip(source),
            &super::ip(destination),
            payload.len(),
            |data| data.copy_from_slice(payload),
            &ChecksumCapabilities::default(),
        );
        buffer
    }

    fn udp_datagram(frame: &[u8]) -> Option<(Ipv4Addr, u16, u16, &[u8])> {
        let ethernet = EthernetFrame::new_checked(frame).ok()?;
        if ethernet.ethertype() != EthernetProtocol::Ipv4 {
            return None;
        }
        let ip = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
        if ip.next_header() != IpProtocol::Udp {
            return None;
        }
        let udp = UdpPacket::new_checked(ip.payload()).ok()?;
        Some((ip.dst_addr(), udp.src_port(), udp.dst_port(), udp.payload()))
    }

    fn arp_reply(frame: &[u8]) -> Option<ArpRepr> {
        let ethernet = EthernetFrame::new_checked(frame).ok()?;
        if ethernet.ethertype() != EthernetProtocol::Arp {
            return None;
        }
        let packet = ArpPacket::new_checked(ethernet.payload()).ok()?;
        ArpRepr::parse(&packet).ok()
    }

    fn arp_request_frame(mac: [u8; 6], source: Ipv4Addr, target: Ipv4Addr) -> Vec<u8> {
        let ethernet = EthernetRepr {
            src_addr: EthernetAddress(mac),
            dst_addr: EthernetAddress::BROADCAST,
            ethertype: EthernetProtocol::Arp,
        };
        let arp = ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Request,
            source_hardware_addr: EthernetAddress(mac),
            source_protocol_addr: source,
            target_hardware_addr: EthernetAddress([0; 6]),
            target_protocol_addr: target,
        };

        let mut buffer = vec![0u8; 14 + arp.buffer_len()];
        ethernet.emit(&mut EthernetFrame::new_unchecked(&mut buffer[..]));
        arp.emit(&mut ArpPacket::new_unchecked(&mut buffer[14..]));
        buffer
    }

    #[allow(clippy::too_many_arguments)]
    fn tcp_repr_frame(
        mac: [u8; 6],
        source: Ipv4Addr,
        destination: Ipv4Addr,
        source_port: u16,
        destination_port: u16,
        sequence: TcpSeqNumber,
        acknowledgement: Option<TcpSeqNumber>,
        payload: &[u8],
    ) -> Vec<u8> {
        let ethernet = EthernetRepr {
            src_addr: EthernetAddress(mac),
            dst_addr: EthernetAddress(ADAPTER_MAC),
            ethertype: EthernetProtocol::Ipv4,
        };
        let control = if !payload.is_empty() {
            TcpControl::Psh
        } else if acknowledgement.is_some() {
            TcpControl::None
        } else {
            TcpControl::Syn
        };
        let tcp = TcpRepr {
            src_port: source_port,
            dst_port: destination_port,
            control,
            seq_number: sequence,
            ack_number: acknowledgement,
            window_len: 8192,
            window_scale: None,
            max_seg_size: Some(1460),
            sack_permitted: false,
            sack_ranges: [None; 3],
            timestamp: None,
            payload,
        };
        let ip = Ipv4Repr {
            src_addr: source,
            dst_addr: destination,
            next_header: IpProtocol::Tcp,
            payload_len: tcp.buffer_len(),
            hop_limit: 64,
        };

        let mut buffer = vec![0u8; 14 + 20 + tcp.buffer_len()];
        ethernet.emit(&mut EthernetFrame::new_unchecked(&mut buffer[..]));
        ip.emit(
            &mut Ipv4Packet::new_unchecked(&mut buffer[14..34]),
            &ChecksumCapabilities::default(),
        );
        tcp.emit(
            &mut TcpPacket::new_unchecked(&mut buffer[34..]),
            &super::ip(source),
            &super::ip(destination),
            &ChecksumCapabilities::default(),
        );
        buffer
    }

    fn tcp_segment(frame: &[u8]) -> Option<(Ipv4Addr, Ipv4Addr, TcpPacket<&[u8]>)> {
        let ethernet = EthernetFrame::new_checked(frame).ok()?;
        if ethernet.ethertype() != EthernetProtocol::Ipv4 {
            return None;
        }
        let ip = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
        if ip.next_header() != IpProtocol::Tcp {
            return None;
        }
        let tcp = TcpPacket::new_checked(ip.payload()).ok()?;
        Some((ip.src_addr(), ip.dst_addr(), tcp))
    }

    fn tcp_frame(
        mac: [u8; 6],
        source: Ipv4Addr,
        destination: Ipv4Addr,
        source_port: u16,
        destination_port: u16,
    ) -> Vec<u8> {
        let mut frame = vec![0u8; 14 + 20 + 20];
        frame[0..6].copy_from_slice(&ADAPTER_MAC);
        frame[6..12].copy_from_slice(&mac);
        frame[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
        frame[14] = 0x45;
        frame[16..18].copy_from_slice(&40u16.to_be_bytes());
        frame[22] = 64;
        frame[23] = IpProtocol::Tcp.into();
        frame[26..30].copy_from_slice(&source.octets());
        frame[30..34].copy_from_slice(&destination.octets());
        frame[34..36].copy_from_slice(&source_port.to_be_bytes());
        frame[36..38].copy_from_slice(&destination_port.to_be_bytes());
        frame[46] = 0x50;
        frame[47] = 0x02;
        frame
    }

    fn dns_query(name: &str, record_type: u16) -> Vec<u8> {
        let mut query = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.push(0);
        query.extend_from_slice(&record_type.to_be_bytes());
        query.extend_from_slice(&1u16.to_be_bytes());
        query
    }
}
