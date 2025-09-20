use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_channel::{Receiver, Sender, bounded};
use futures::{SinkExt, StreamExt};
use native_tls::{Certificate as NativeTlsCertificate, TlsConnector};
use reqwest::Certificate as HttpCertificate;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async, connect_async_tls_with_config,
};
use tokio_util::codec::{Framed, LinesCodec};
use tracing::{info, warn};
use url::Url;

use crate::error::{CodexErr, Result as CodexResult};
use crate::protocol::{Event, Op, SessionConfiguredEvent, Submission};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct RemoteSpawnParams {
    pub remote: Url,
    pub sse_base: Url,
    pub token: Option<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub trust_cert: Option<Vec<u8>>,
}

pub(crate) struct RemoteSpawnedConversation {
    pub conversation: RemoteConversation,
    pub session_configured: SessionConfiguredEvent,
}

pub(crate) struct RemoteConversation {
    conversation_id: String,
    rpc: Arc<Mutex<RpcClient>>,
    event_rx: Receiver<Event>,
    shutdown: Arc<AtomicBool>,
    event_task: Option<JoinHandle<()>>,
}

impl RemoteConversation {
    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    pub(crate) async fn submit(&self, op: Op) -> CodexResult<String> {
        let params = json!({
            "conversation_id": self.conversation_id,
            "op": op,
            "echo_user": true,
        });
        let mut client = self.rpc.lock().await;
        let value = client.request("submit", params).await?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CodexErr::RemoteTransport("submit response missing id".to_string()))?;
        Ok(id.to_string())
    }

    pub(crate) async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        let params = json!({
            "conversation_id": self.conversation_id,
            "sub": sub,
        });
        let mut client = self.rpc.lock().await;
        let _ = client.request("submit_with_id", params).await?;
        Ok(())
    }

    pub(crate) async fn next_event(&self) -> CodexResult<Event> {
        match self.event_rx.recv().await {
            Ok(event) => Ok(event),
            Err(_) => Err(CodexErr::RemoteStreamClosed),
        }
    }
}

impl Drop for RemoteConversation {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.event_task.take() {
            handle.abort();
        }
    }
}

pub(crate) async fn spawn_remote_conversation(
    params: RemoteSpawnParams,
) -> CodexResult<RemoteSpawnedConversation> {
    let timeout = params.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let trust = params.trust_cert.clone();
    let mut client = RpcClient::connect(&params.remote, timeout, trust.clone()).await?;

    let mut spawn_params = serde_json::Map::new();
    if let Some(cwd) = params.cwd.clone() {
        spawn_params.insert(
            "cwd".to_string(),
            Value::String(cwd.to_string_lossy().into_owned()),
        );
    }
    if let Some(token) = params.token.clone() {
        spawn_params.insert("token".to_string(), Value::String(token));
    }

    let value = client.request("spawn", Value::Object(spawn_params)).await?;

    #[derive(Deserialize)]
    struct SpawnResponse {
        conversation_id: String,
        session_configured: SessionConfiguredEvent,
    }

    let spawn: SpawnResponse = serde_json::from_value(value)
        .map_err(|e| CodexErr::RemoteTransport(format!("invalid spawn response: {e}")))?;

    let (event_tx, event_rx) = bounded::<Event>(256);
    let shutdown = Arc::new(AtomicBool::new(false));
    let stream_opts = EventStreamOptions {
        sse_base: params.sse_base,
        token: params.token,
        trust_cert: params.trust_cert,
    };

    let conversation_id = spawn.conversation_id.clone();
    let shutdown_clone = shutdown.clone();
    let event_task = tokio::spawn(async move {
        stream_events(conversation_id, stream_opts, event_tx, shutdown_clone).await;
    });

    let conversation = RemoteConversation {
        conversation_id: spawn.conversation_id,
        rpc: Arc::new(Mutex::new(client)),
        event_rx,
        shutdown,
        event_task: Some(event_task),
    };

    Ok(RemoteSpawnedConversation {
        conversation,
        session_configured: spawn.session_configured,
    })
}

struct EventStreamOptions {
    sse_base: Url,
    token: Option<String>,
    trust_cert: Option<Vec<u8>>,
}

async fn stream_events(
    conversation_id: String,
    opts: EventStreamOptions,
    sender: Sender<Event>,
    shutdown: Arc<AtomicBool>,
) {
    let mut last_seq: u64 = 1;

    while !shutdown.load(Ordering::Relaxed) {
        match open_sse_stream(&conversation_id, last_seq, &opts).await {
            Ok(response) => {
                info!(target: "remote", "connected SSE stream for conversation {conversation_id}");
                let mut buffer: Vec<u8> = Vec::new();
                let mut data_lines: Vec<String> = Vec::new();
                let mut stream = response.bytes_stream();

                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(bytes) => {
                            buffer.extend_from_slice(&bytes);
                            while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                                let mut line = buffer.drain(..=pos).collect::<Vec<u8>>();
                                if line.ends_with(&[b'\n']) {
                                    line.pop();
                                }
                                if line.ends_with(&[b'\r']) {
                                    line.pop();
                                }
                                if line.is_empty() {
                                    if data_lines.is_empty() {
                                        continue;
                                    }
                                    let joined = data_lines.join("\n");
                                    data_lines.clear();
                                    let trimmed = joined.trim();
                                    if trimmed.is_empty()
                                        || trimmed.eq_ignore_ascii_case("keepalive")
                                    {
                                        continue;
                                    }
                                    let value: Value = match serde_json::from_str(trimmed) {
                                        Ok(v) => v,
                                        Err(err) => {
                                            warn!(
                                                target: "remote",
                                                "ignoring malformed SSE payload: {err}"
                                            );
                                            continue;
                                        }
                                    };
                                    let seq = value
                                        .get("event_seq")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(last_seq);
                                    let event: Event = match serde_json::from_value(value.clone()) {
                                        Ok(ev) => ev,
                                        Err(err) => {
                                            warn!(
                                                target: "remote",
                                                "failed to decode SSE event: {err}"
                                            );
                                            continue;
                                        }
                                    };
                                    last_seq = seq.max(1);
                                    if sender.send(event).await.is_err() {
                                        return;
                                    }
                                } else if line.starts_with(b":") {
                                    continue;
                                } else if line.starts_with(b"data:") {
                                    let data = &line[5..];
                                    match String::from_utf8(data.to_vec()) {
                                        Ok(text) => {
                                            if let Some(rest) = text.strip_prefix(' ') {
                                                data_lines.push(rest.to_string());
                                            } else {
                                                data_lines.push(text);
                                            }
                                        }
                                        Err(err) => {
                                            warn!(
                                                target: "remote",
                                                "invalid UTF-8 in SSE data: {err}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            warn!(target: "remote", "SSE stream error: {err}");
                            break;
                        }
                    }

                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                }
            }
            Err(err) => {
                warn!(target: "remote", "failed to connect SSE stream: {err}");
            }
        }

        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    sender.close();
}

async fn open_sse_stream(
    conversation_id: &str,
    from_seq: u64,
    opts: &EventStreamOptions,
) -> Result<reqwest::Response, String> {
    let mut url = opts
        .sse_base
        .join(&format!("sessions/{conversation_id}/events"))
        .map_err(|e| format!("invalid SSE base: {e}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("from_seq", &from_seq.to_string());
        if let Some(token) = &opts.token {
            pairs.append_pair("token", token);
        }
    }

    let mut builder = reqwest::Client::builder();
    builder = builder.redirect(reqwest::redirect::Policy::none());
    if url.scheme() == "https" {
        if let Some(bytes) = opts.trust_cert.clone() {
            match HttpCertificate::from_pem(&bytes) {
                Ok(cert) => {
                    builder = builder
                        .danger_accept_invalid_certs(true)
                        .add_root_certificate(cert);
                }
                Err(err) => {
                    return Err(format!("failed to parse trust cert: {err}"));
                }
            }
        }
    }

    let client = builder
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let mut request = client.get(url).header("Accept", "text/event-stream");
    if let Some(token) = &opts.token {
        request = request
            .header("X-Smarty-Token", token)
            .header("Authorization", format!("Bearer {token}"));
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("failed to open SSE stream: {e}"))?
        .error_for_status()
        .map_err(|e| format!("SSE endpoint returned error: {e}"))?;

    Ok(response)
}

struct RpcClient {
    transport: Transport,
    next_id: u64,
    timeout: Duration,
}

enum Transport {
    Ws(WebSocketStream<MaybeTlsStream<TcpStream>>),
    Tcp(Framed<TcpStream, LinesCodec>),
}

impl RpcClient {
    async fn connect(
        remote: &Url,
        timeout: Duration,
        trust_cert: Option<Vec<u8>>,
    ) -> CodexResult<Self> {
        let transport = match remote.scheme() {
            "ws" | "wss" => {
                let connector = if remote.scheme() == "wss" {
                    let mut builder = TlsConnector::builder();
                    if let Some(bytes) = trust_cert.clone() {
                        match NativeTlsCertificate::from_pem(&bytes) {
                            Ok(cert) => {
                                builder.add_root_certificate(cert);
                                builder.danger_accept_invalid_certs(true);
                                builder.danger_accept_invalid_hostnames(true);
                            }
                            Err(err) => {
                                return Err(CodexErr::RemoteTransport(format!(
                                    "failed to parse trust cert: {err}"
                                )));
                            }
                        }
                    }
                    match builder.build() {
                        Ok(conn) => Some(Connector::NativeTls(conn)),
                        Err(err) => {
                            return Err(CodexErr::RemoteTransport(format!(
                                "failed to build TLS connector: {err}"
                            )));
                        }
                    }
                } else {
                    None
                };

                let url = remote.to_string();
                let fut = async {
                    match connector {
                        Some(conn) => {
                            connect_async_tls_with_config(url.as_str(), None, false, Some(conn))
                                .await
                        }
                        None => connect_async(url.as_str()).await,
                    }
                };
                let (stream, _) = tokio::time::timeout(timeout, fut)
                    .await
                    .map_err(|_| {
                        CodexErr::RemoteTransport("timeout establishing websocket".into())
                    })?
                    .map_err(|e| {
                        CodexErr::RemoteTransport(format!("websocket handshake error: {e}"))
                    })?;
                Transport::Ws(stream)
            }
            "tcp" => {
                let host = remote
                    .host_str()
                    .ok_or_else(|| CodexErr::RemoteTransport("tcp URL missing host".into()))?;
                let port = remote
                    .port()
                    .ok_or_else(|| CodexErr::RemoteTransport("tcp URL missing port".into()))?;
                let addr = format!("{host}:{port}");
                let stream = tokio::time::timeout(timeout, TcpStream::connect(addr))
                    .await
                    .map_err(|_| {
                        CodexErr::RemoteTransport("timeout connecting to tcp endpoint".into())
                    })?
                    .map_err(|e| {
                        CodexErr::RemoteTransport(format!("failed to connect to tcp endpoint: {e}"))
                    })?;
                Transport::Tcp(Framed::new(stream, LinesCodec::new()))
            }
            other => {
                return Err(CodexErr::RemoteTransport(format!(
                    "unsupported remote scheme: {other}"
                )));
            }
        };

        Ok(Self {
            transport,
            next_id: 1,
            timeout,
        })
    }

    async fn request(&mut self, method: &str, params: Value) -> CodexResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let id_str = id.to_string();
        let payload = json!({
            "id": id_str,
            "method": method,
            "params": params,
        });
        let text = serde_json::to_string(&payload)?;
        match &mut self.transport {
            Transport::Ws(stream) => {
                stream
                    .send(Message::Text(text))
                    .await
                    .map_err(|e| CodexErr::RemoteTransport(format!("ws send failed: {e}")))?;
            }
            Transport::Tcp(framed) => {
                framed
                    .send(text)
                    .await
                    .map_err(|e| CodexErr::RemoteTransport(format!("tcp send failed: {e}")))?;
            }
        }

        loop {
            let value = self.recv_json().await?;
            let msg_id = value.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            if msg_id != id_str {
                continue;
            }
            if let Some(err) = value.get("error") {
                let message = match err {
                    Value::String(s) => s.clone(),
                    Value::Object(map) => map
                        .get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| err.to_string()),
                    _ => err.to_string(),
                };
                return Err(CodexErr::RemoteTransport(message));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn recv_json(&mut self) -> CodexResult<Value> {
        match &mut self.transport {
            Transport::Ws(stream) => loop {
                let next = tokio::time::timeout(self.timeout, stream.next())
                    .await
                    .map_err(|_| {
                        CodexErr::RemoteTransport("timeout waiting for websocket message".into())
                    })?;
                let msg = next.transpose().map_err(|e| {
                    CodexErr::RemoteTransport(format!("websocket receive error: {e}"))
                })?;
                let msg = msg.ok_or_else(|| {
                    CodexErr::RemoteTransport("websocket closed unexpectedly".into())
                })?;
                match msg {
                    Message::Text(text) => {
                        return serde_json::from_str(&text).map_err(CodexErr::from);
                    }
                    Message::Binary(bin) => {
                        if let Ok(text) = String::from_utf8(bin) {
                            if let Ok(val) = serde_json::from_str(&text) {
                                return Ok(val);
                            }
                        }
                    }
                    Message::Ping(data) => {
                        stream.send(Message::Pong(data)).await.map_err(|e| {
                            CodexErr::RemoteTransport(format!("failed to respond to ping: {e}"))
                        })?;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(frame) => {
                        return Err(CodexErr::RemoteTransport(format!(
                            "websocket closed: {:?}",
                            frame
                        )));
                    }
                    _ => continue,
                }
            },
            Transport::Tcp(framed) => loop {
                let msg = tokio::time::timeout(self.timeout, framed.next())
                    .await
                    .map_err(|_| {
                        CodexErr::RemoteTransport("timeout waiting for tcp frame".into())
                    })?;
                match msg {
                    Some(Ok(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        return serde_json::from_str(&line).map_err(CodexErr::from);
                    }
                    Some(Err(err)) => {
                        return Err(CodexErr::RemoteTransport(format!(
                            "tcp stream error: {err}"
                        )));
                    }
                    None => return Err(CodexErr::RemoteTransport("tcp stream closed".into())),
                }
            },
        }
    }
}
