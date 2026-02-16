use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::config::Config;
use crate::events::NfcEvent;

type ClientId = u64;
type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    Message,
>;
type ClientMap = Arc<Mutex<HashMap<ClientId, WsSink>>>;

/// Run the WebSocket server. Accepts browser connections and broadcasts NFC events.
pub async fn run_ws_server(
    config: &Config,
    mut event_rx: broadcast::Receiver<NfcEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = config.addr();
    let listener = TcpListener::bind(&addr).await?;
    info!("WebSocket server listening on ws://{}", addr);

    let clients: ClientMap = Arc::new(Mutex::new(HashMap::new()));
    let mut next_id: ClientId = 0;

    // Spawn a task to broadcast NFC events to all connected clients
    let broadcast_clients = clients.clone();
    tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let json = match serde_json::to_string(&event) {
                Ok(j) => j,
                Err(e) => {
                    warn!("Failed to serialize event: {}", e);
                    continue;
                }
            };

            let mut map = broadcast_clients.lock().await;
            let mut dead_clients = vec![];

            for (id, sink) in map.iter_mut() {
                if sink.send(Message::Text(json.clone().into())).await.is_err() {
                    dead_clients.push(*id);
                }
            }

            for id in dead_clients {
                map.remove(&id);
                info!("Client {} removed (send failed)", id);
            }
        }
    });

    // Accept new WebSocket connections
    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let ws_stream = match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => ws,
            Err(e) => {
                warn!("WebSocket handshake failed from {}: {}", peer_addr, e);
                continue;
            }
        };

        let client_id = next_id;
        next_id += 1;
        info!("Client {} connected from {}", client_id, peer_addr);

        let (sink, mut incoming) = ws_stream.split();
        clients.lock().await.insert(client_id, sink);

        // Spawn a task to handle incoming messages (ping/pong, close)
        let disconnect_clients = clients.clone();
        tokio::spawn(async move {
            while let Some(msg) = incoming.next().await {
                match msg {
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Ping(_)) => {
                        // Pong is handled automatically by tungstenite
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
            disconnect_clients.lock().await.remove(&client_id);
            info!("Client {} disconnected", client_id);
        });
    }
}
