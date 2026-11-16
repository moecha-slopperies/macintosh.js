use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::{net::UdpSocket, sync::Notify};

const UDPTUNNEL_PORT: u16 = 6066;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EthernetFrame {
    frame: Vec<u8>,
}

pub struct EthernetState {
    socket: Mutex<Option<Arc<UdpSocket>>>,
    stop: Mutex<Option<Arc<Notify>>>,
}

impl EthernetState {
    pub fn new() -> Self {
        Self {
            socket: Mutex::new(None),
            stop: Mutex::new(None),
        }
    }

    pub async fn close(&self) {
        let socket = self
            .socket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let stop = self
            .stop
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(stop) = stop {
            stop.notify_waiters();
        }
        if let Some(socket) = socket {
            let _ = socket
                .send_to(&[], ("127.255.255.255", UDPTUNNEL_PORT))
                .await;
        }
    }
}

#[tauri::command]
pub async fn ethernet_init(
    app: AppHandle,
    mac_address: String,
    state: tauri::State<'_, EthernetState>,
) -> Result<(), String> {
    state.close().await;

    let socket = UdpSocket::bind(("0.0.0.0", UDPTUNNEL_PORT))
        .await
        .map_err(|error| format!("could not bind udp/{UDPTUNNEL_PORT}: {error}"))?;
    socket
        .set_broadcast(true)
        .map_err(|error| format!("could not enable UDP broadcast: {error}"))?;
    let socket = Arc::new(socket);
    let stop = Arc::new(Notify::new());
    *state
        .socket
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket.clone());
    *state
        .stop
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stop.clone());

    let own_mac = mac_address
        .split(':')
        .filter_map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Vec<_>>();
    println!("ethernet: {mac_address} listening on udp/{UDPTUNNEL_PORT} (udptunnel)");

    tauri::async_runtime::spawn(async move {
        let mut buffer = vec![0; 65_536];
        loop {
            tokio::select! {
                _ = stop.notified() => break,
                result = socket.recv(&mut buffer) => {
                    let Ok(received) = result else { continue };
                    let frame = &buffer[..received];
                    if own_mac.len() == 6 && frame.len() >= 12 && frame[6..12] == own_mac {
                        continue;
                    }
                    let _ = app.emit_to("main", "ethernet-receive", EthernetFrame { frame: frame.to_vec() });
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn ethernet_send(
    _destination: String,
    frame: Vec<u8>,
    state: tauri::State<'_, EthernetState>,
) -> Result<(), String> {
    let socket = state
        .socket
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some(socket) = socket else { return Ok(()) };
    socket
        .send_to(&frame, ("255.255.255.255", UDPTUNNEL_PORT))
        .await
        .map_err(|error| format!("UDP send failed: {error}"))?;
    Ok(())
}
