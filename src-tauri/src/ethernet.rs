use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter};
use tokio::{net::UdpSocket, sync::Notify};

const UDPTUNNEL_PORT: u16 = 6066;

// Ethernet II header through the largest frame the emulator's `ether js`
// driver hands to `etherRead`.
const MIN_FRAME_LEN: usize = 14;
const MAX_FRAME_LEN: usize = 1514;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EthernetFrame {
    frame: Vec<u8>,
}

struct Bridge {
    socket: Arc<UdpSocket>,
    stop: Arc<Notify>,
    task: JoinHandle<()>,
}

pub struct EthernetState {
    bridge: Mutex<Option<Bridge>>,
}

impl EthernetState {
    pub fn new() -> Self {
        Self {
            bridge: Mutex::new(None),
        }
    }

    pub async fn close(&self) {
        let bridge = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(Bridge { socket, stop, task }) = bridge else {
            return;
        };

        stop.notify_one();

        let _ = task.await;
        drop(socket);
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

fn accepts_frame(frame: &[u8], own_mac: &[u8; 6]) -> bool {
    is_valid_frame_len(frame.len()) && frame[6..12] != own_mac[..]
}

#[tauri::command]
pub async fn ethernet_init(
    app: AppHandle,
    mac_address: String,
    state: tauri::State<'_, EthernetState>,
) -> Result<(), String> {
    let own_mac = parse_mac(&mac_address)?;
    state.close().await;

    let socket = UdpSocket::bind(("0.0.0.0", UDPTUNNEL_PORT))
        .await
        .map_err(|error| format!("could not bind udp/{UDPTUNNEL_PORT}: {error}"))?;
    socket
        .set_broadcast(true)
        .map_err(|error| format!("could not enable UDP broadcast: {error}"))?;
    let socket = Arc::new(socket);
    let stop = Arc::new(Notify::new());
    let receive_socket = socket.clone();
    let receive_stop = stop.clone();

    // UDPTUNNEL is an unauthenticated L2 bus: any peer that can reach this
    // port can inject frames. The length and source-MAC checks only keep
    // malformed frames and our own broadcasts out of the guest.
    let task = tauri::async_runtime::spawn(async move {
        let mut buffer = vec![0; 65_536];
        loop {
            tokio::select! {
                _ = receive_stop.notified() => break,
                result = receive_socket.recv(&mut buffer) => {
                    let received = match result {
                        Ok(received) => received,
                        Err(_) => {
                            // A transient UDP error (for example an ICMP
                            // unreachable surfacing on Windows) must not spin
                            // the loop.
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                    };
                    let frame = &buffer[..received];
                    if !accepts_frame(frame, &own_mac) {
                        continue;
                    }
                    let _ = app.emit_to(
                        "main",
                        "ethernet-receive",
                        EthernetFrame {
                            frame: frame.to_vec(),
                        },
                    );
                }
            }
        }
    });

    *state
        .bridge
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Bridge { socket, stop, task });

    println!("ethernet: {mac_address} listening on udp/{UDPTUNNEL_PORT} (udptunnel)");
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
    let socket = state
        .bridge
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|bridge| bridge.socket.clone());
    let Some(socket) = socket else { return Ok(()) };
    socket
        .send_to(&frame, ("255.255.255.255", UDPTUNNEL_PORT))
        .await
        .map_err(|error| format!("UDP send failed: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{accepts_frame, is_valid_frame_len, parse_mac};

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
    fn accepts_frames_from_other_sources() {
        let own_mac = [0x02, 0, 0, 0, 0, 1];
        let mut frame = vec![0u8; 14];
        frame[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        assert!(accepts_frame(&frame, &own_mac));
    }

    #[test]
    fn rejects_own_frames() {
        let own_mac = [0x02, 0, 0, 0, 0, 1];
        let mut frame = vec![0u8; 14];
        frame[6..12].copy_from_slice(&own_mac);
        assert!(!accepts_frame(&frame, &own_mac));
    }

    #[test]
    fn checks_frame_length_bounds() {
        assert!(!is_valid_frame_len(13));
        assert!(is_valid_frame_len(14));
        assert!(is_valid_frame_len(1514));
        assert!(!is_valid_frame_len(1515));
    }

    #[test]
    fn rejects_frames_outside_the_driver_limits() {
        let own_mac = [0x02, 0, 0, 0, 0, 1];

        let mut short = vec![0u8; 13];
        short[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        assert!(!accepts_frame(&short, &own_mac));

        let mut oversized = vec![0u8; 1515];
        oversized[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        assert!(!accepts_frame(&oversized, &own_mac));

        let mut largest = vec![0u8; 1514];
        largest[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        assert!(accepts_frame(&largest, &own_mac));
    }
}
