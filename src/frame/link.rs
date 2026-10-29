//! Connections to the TV. The remote channel stays open between keys and is
//! reopened when the Frame drops it. Art and device info are per request.

use super::protocol::{self, ArtReply, DeviceInfo};
use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use std::{net::UdpSocket, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{client_async, tungstenite::Message, WebSocketStream};

#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn tls() -> Result<TlsConnector> {
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn connect(
    host: &str,
    channel: &str,
    token: &str,
) -> Result<WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>> {
    let tcp = timeout(Duration::from_secs(4), TcpStream::connect((host, 8002)))
        .await
        .context("the TV did not answer on port 8002")?
        .with_context(|| format!("cannot reach the TV at {host}:8002"))?;
    let name =
        ServerName::try_from(host.to_owned()).map_err(|error| anyhow!("bad host: {error}"))?;
    let tls_stream = tls()?
        .connect(name, tcp)
        .await
        .context("TV TLS handshake failed")?;
    let path = protocol::channel_path(channel, token);
    let url = format!("wss://{host}:8002{path}");
    let (socket, _) = timeout(Duration::from_secs(6), client_async(url, tls_stream))
        .await
        .context("the TV did not accept the remote connection")?
        .context("TV websocket failed")?;
    Ok(socket)
}

async fn read_text<S>(socket: &mut WebSocketStream<S>, wait: Duration) -> Result<Option<String>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match timeout(wait, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            // Connect messages carry the pairing token. Keep it out of logs.
            if !text.contains("\"token\"") {
                log::debug!("tv: {text}");
            }
            Ok(Some(text.to_string()))
        }
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => Ok(None),
        Ok(Some(Ok(_))) => Ok(None),
        Ok(Some(Err(error))) => Err(anyhow!(error)),
        Err(_) => Ok(None),
    }
}

/// Hold the remote channel open until the TV returns a token. Press Allow on the screen.
pub async fn pair(host: &str) -> Result<String> {
    let mut socket = connect(host, protocol::REMOTE_CHANNEL, "").await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while tokio::time::Instant::now() < deadline {
        let wait = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Some(text) = read_text(&mut socket, wait).await? else {
            continue;
        };
        if let Some(token) = protocol::parse_token(&text) {
            return Ok(token);
        }
    }
    anyhow::bail!("the TV did not hand back a token. Turn it on and press Allow")
}

type Socket = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

/// The remote channel, kept open between keys. Opening it costs a TLS
/// handshake and the TV's greeting, most of a key's delay. A Frame drops an
/// idle session, so a dead socket is reopened.
#[derive(Default)]
pub struct Remote(Option<Socket>);

impl Remote {
    /// Holds a key down for `hold`, or clicks it when `hold` is `None`. A
    /// Frame only powers fully off on a held power key; a click toggles art.
    /// Returns a new token if the TV handed one back.
    pub async fn press(
        &mut self,
        host: &str,
        token: &str,
        key: &str,
        hold: Option<Duration>,
    ) -> Result<Option<String>> {
        let mut found = None;
        if let Some(socket) = self.0.as_mut() {
            if !alive(socket).await {
                self.0 = None;
            }
        }
        for attempt in 0..2 {
            if self.0.is_none() {
                let (socket, token) = open_remote(host, token).await?;
                found = found.or(token);
                self.0 = Some(socket);
            }
            let socket = self.0.as_mut().expect("opened above");
            match send_key(socket, key, hold).await {
                Ok(()) => return Ok(found),
                Err(error) if attempt == 0 => {
                    log::debug!("remote channel dropped, reopening: {error:#}");
                    self.0 = None;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("the second attempt returns")
    }
}

/// Opens the remote channel and reads the TV's greeting, which carries a
/// token only when the TV issues a new one.
async fn open_remote(host: &str, token: &str) -> Result<(Socket, Option<String>)> {
    let mut socket = connect(host, protocol::REMOTE_CHANNEL, token).await?;
    while let Some(text) = read_text(&mut socket, Duration::from_secs(3)).await? {
        if text.contains("\"ms.channel.connect\"") {
            return Ok((socket, protocol::parse_token(&text)));
        }
    }
    anyhow::bail!("the TV did not open the remote channel")
}

/// Drains what the TV sent since the last key. False if it hung up.
async fn alive(socket: &mut Socket) -> bool {
    loop {
        match timeout(Duration::from_millis(5), socket.next()).await {
            Err(_) => return true,
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => return false,
            Ok(Some(Ok(_))) => {}
        }
    }
}

async fn send_key(socket: &mut Socket, key: &str, hold: Option<Duration>) -> Result<()> {
    let send = |payload: String| Message::Text(payload.into());
    match hold {
        None => socket.send(send(protocol::key_payload(key))).await?,
        Some(hold) => {
            // A 2024 Frame reads one Press as a short press. A physical remote
            // repeats Press while the button is down, so do the same.
            let until = tokio::time::Instant::now() + hold;
            while tokio::time::Instant::now() < until {
                socket
                    .send(send(protocol::key_command("Press", key)))
                    .await?;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            socket
                .send(send(protocol::key_command("Release", key)))
                .await?;
        }
    }
    Ok(())
}

pub async fn art(
    host: &str,
    token: &str,
    request: &str,
    value: Option<&str>,
) -> Result<Vec<ArtReply>> {
    let mut socket = connect(host, protocol::ART_CHANNEL, token).await?;
    // The TV answers "No Authorized" to anything sent before the channel is ready.
    wait_ready(&mut socket).await?;
    let id = request_id();
    socket
        .send(Message::Text(
            protocol::art_payload(request, value, &id).into(),
        ))
        .await
        .context("could not send the art command")?;
    let mut replies = Vec::new();
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let Some(text) = read_text(&mut socket, Duration::from_secs(3)).await? else {
            break;
        };
        if let Some(reply) = protocol::parse_art_message(&text) {
            let answered = matches!(reply, ArtReply::Mode(_));
            replies.push(reply);
            if answered {
                break;
            }
        }
    }
    let _ = socket.close(None).await;
    Ok(replies)
}

async fn wait_ready<S>(socket: &mut WebSocketStream<S>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let wait = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Some(text) = read_text(socket, wait).await? else {
            continue;
        };
        if text.contains("\"ms.channel.ready\"") {
            return Ok(());
        }
    }
    anyhow::bail!("the TV did not open the channel")
}

fn request_id() -> String {
    let bytes: [u8; 16] = rand::random();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

pub async fn device_info(host: &str) -> Result<DeviceInfo> {
    if let Ok(body) = https_get(host, 8002, "/api/v2/").await {
        if let Some(info) = protocol::parse_device_info(&body) {
            return Ok(info);
        }
    }
    let body = http_get(host, 8001, "/api/v2/")
        .await
        .context("the TV did not answer. It may be fully off, or IP control is disabled")?;
    protocol::parse_device_info(&body).context("the TV answered, but not with device info")
}

pub fn wake(mac: &str) -> Result<()> {
    let mac = protocol::parse_mac(mac)
        .context("the TV's MAC address is missing. Pair it while it is on")?;
    let packet = protocol::magic_packet(mac);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_broadcast(true)?;
    for port in [9u16, 7] {
        let _ = socket.send_to(&packet, ("255.255.255.255", port));
    }
    Ok(())
}

async fn https_get(host: &str, port: u16, path: &str) -> Result<String> {
    let tcp = timeout(Duration::from_secs(4), TcpStream::connect((host, port)))
        .await
        .context("timed out")??;
    let name = ServerName::try_from(host.to_owned()).map_err(|error| anyhow!("{error}"))?;
    let mut stream = tls()?.connect(name, tcp).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(4), stream.read_to_end(&mut bytes)).await??;
    Ok(body(&bytes))
}

async fn http_get(host: &str, port: u16, path: &str) -> Result<String> {
    let mut stream = timeout(Duration::from_secs(4), TcpStream::connect((host, port)))
        .await
        .context("timed out")??;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(4), stream.read_to_end(&mut bytes)).await??;
    Ok(body(&bytes))
}

fn body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_else(|| text.into_owned())
}
