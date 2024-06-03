use core::fmt;
use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use futures::{stream::SplitStream, SinkExt, StreamExt};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_util::codec::{Framed, LinesCodec};
use tracing::{info, level_filters::LevelFilter, warn};
use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, util::SubscriberInitExt, Layer as _};

const SERVER_ADDR: &str = "0.0.0.0:9090";
const MAX_MESSAGE: usize = 128;

#[derive(Debug, Default)]
struct AppState {
    peers: DashMap<SocketAddr, mpsc::Sender<Arc<Message>>>,
}

#[derive(Debug)]
enum Message {
    UserJoined(String),
    UserLeft(String),
    Chat { sender: String, content: String },
}

#[derive(Debug)]
struct Peer {
    username: String,
    stream: SplitStream<Framed<TcpStream, LinesCodec>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let layer = Layer::new().with_filter(LevelFilter::INFO);
    tracing_subscriber::registry().with(layer).init();

    let state = Arc::new(AppState::default());

    let listener = TcpListener::bind(SERVER_ADDR).await?;
    info!("Listening on {}", SERVER_ADDR);

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        info!("Accepted connection from {}", remote_addr);

        let state_cloned = state.clone();
        // Spawn a new task to handle the connection
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, remote_addr, state_cloned).await {
                warn!("Error handling connection: {}", e);
            };
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    remote_addr: SocketAddr,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    let mut stream = Framed::new(stream, LinesCodec::new());

    // Ask peer to enter username.
    stream.send("Enter your username:").await?;
    let username = match stream.next().await {
        Some(Ok(v)) => v,
        Some(Err(e)) => return Err(e.into()),
        None => return Ok(()),
    };

    // Add peer to state.
    let mut peer = state.add(username, remote_addr, stream).await;
    let message = Message::user_joined(&peer.username);
    info!("{}", message);
    state.broadcast(remote_addr, Arc::new(message)).await;

    // Handle incoming messages.
    while let Some(Ok(line)) = peer.stream.next().await {
        let message = Message::chat(&peer.username, line);
        state.broadcast(remote_addr, Arc::new(message)).await;
    }

    // Remove peer from state.
    state.peers.remove(&remote_addr);
    let message = Message::user_left(&peer.username);
    info!("{}", message);
    state.broadcast(remote_addr, Arc::new(message)).await;

    Ok(())
}

impl Message {
    fn user_joined(username: &str) -> Self {
        let content = format!("{} joined the chat", username);
        Self::UserJoined(content)
    }

    fn user_left(username: &str) -> Self {
        let content = format!("{} left the chat", username);
        Self::UserLeft(content)
    }

    fn chat(sender: &str, content: String) -> Self {
        let content = format!("{}: {}", sender, content);
        Self::Chat {
            sender: sender.to_string(),
            content,
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::UserJoined(content) => write!(f, "{}", content),
            Message::UserLeft(content) => write!(f, "{}", content),
            Message::Chat { sender, content } => {
                write!(f, "{}: {}", sender, content)
            }
        }
    }
}

impl AppState {
    async fn broadcast(&self, addr: SocketAddr, message: Arc<Message>) {
        for peer in &self.peers {
            if peer.key() == &addr {
                continue;
            }
            if let Err(e) = peer.send(message.clone()).await {
                warn!("Failed to send message to {}: {}", peer.key(), e);

                // Remove the peer if the send fails
                self.peers.remove(peer.key());
            }
        }
    }

    async fn add(
        &self,
        username: String,
        addr: SocketAddr,
        stream: Framed<TcpStream, LinesCodec>,
    ) -> Peer {
        // Create a channel for each peer to send messages and receive messages.
        let (tx, mut rx) = mpsc::channel(MAX_MESSAGE);

        // Save the sender in the state so that we can broadcast messages to all peers.
        self.peers.insert(addr, tx);

        let (mut stream_sender, stream_receiver) = stream.split();

        // Spawn a task to handle incoming messages from the peer.
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let Err(e) = stream_sender.send(message.to_string()).await {
                    warn!("Failed to send message to {}: {}", addr, e);
                    break;
                }
            }
        });

        Peer {
            username,
            stream: stream_receiver,
        }
    }
}
