use aead::{generic_array::GenericArray, AeadInPlace, NewAead};
use bytes::{Buf, BytesMut};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, Tag};
use futures::{
    channel::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    io::Error,
    Stream,
};
use log::{debug, error};
use std::{
    cmp::min,
    future::Future,
    io::{self, ErrorKind},
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
    task::{ready, Context, Poll, Waker},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use uuid::Uuid;

use crate::Result;

#[derive(Debug)]
pub struct StreamWrapper {
    incoming_receiver: UnboundedReceiver<Vec<u8>>,
    outgoing_sender: UnboundedSender<Vec<u8>>,
    incoming_waker: Arc<Mutex<Option<Waker>>>,
    outgoing_waker: Arc<Mutex<Option<Waker>>>,
    incoming_buf: BytesMut,
}

impl StreamWrapper {
    pub fn new(
        incoming_receiver: UnboundedReceiver<Vec<u8>>,
        outgoing_sender: UnboundedSender<Vec<u8>>,
        incoming_waker: Arc<Mutex<Option<Waker>>>,
        outgoing_waker: Arc<Mutex<Option<Waker>>>,
    ) -> StreamWrapper {
        StreamWrapper {
            incoming_receiver,
            outgoing_sender,
            incoming_waker,
            outgoing_waker,
            incoming_buf: BytesMut::new(),
        }
    }

    fn poll_receiver(&mut self, cx: &mut Context) -> Poll<usize> {
        debug!("polling incoming TCP stream receiver");

        match Stream::poll_next(Pin::new(&mut self.incoming_receiver), cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(incoming)) => {
                let r_len = incoming.len();
                self.incoming_buf.extend_from_slice(&incoming);

                debug!("received {} Bytes on incoming TCP stream receiver", &r_len);

                Poll::Ready(r_len)
            },
            Poll::Ready(None) => {
                debug!("received 0 Bytes on incoming TCP stream receiver");
                Poll::Ready(0)
            },
        }
    }
}

impl AsyncRead for StreamWrapper {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::result::Result<(), io::Error>> {
        let stream_wrapper = Pin::into_inner(self);

        match stream_wrapper.poll_receiver(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(_r_len) => {
                let r_len = min(buf.capacity(), stream_wrapper.incoming_buf.len());
                buf.put_slice(&stream_wrapper.incoming_buf[..r_len]);
                stream_wrapper.incoming_buf.advance(r_len);

                if let Some(waker) = stream_wrapper
                    .outgoing_waker
                    .lock()
                    .expect("accessing outgoing_waker")
                    .take()
                {
                    waker.wake()
                }
                if let Some(waker) = stream_wrapper
                    .incoming_waker
                    .lock()
                    .expect("accessing incoming_waker")
                    .take()
                {
                    waker.wake()
                }

                Poll::Ready(Ok(()))
            },
        }
    }
}

impl AsyncWrite for StreamWrapper {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context, buf: &[u8]) -> Poll<std::result::Result<usize, io::Error>> {
        let stream_wrapper = Pin::into_inner(self);

        debug!("writing {} Bytes to outgoing TCP stream sender", buf.len());

        stream_wrapper
            .outgoing_sender
            .unbounded_send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "couldn't write"))?;

        if let Some(waker) = stream_wrapper
            .outgoing_waker
            .lock()
            .expect("accessing outgoing_waker")
            .take()
        {
            waker.wake()
        }
        if let Some(waker) = stream_wrapper
            .incoming_waker
            .lock()
            .expect("accessing incoming_waker")
            .take()
        {
            waker.wake()
        }

        let w_len = buf.len();

        debug!("wrote {} Bytes to outgoing TCP stream sender", &w_len);

        Poll::Ready(Ok(w_len))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<std::result::Result<(), io::Error>> {
        // let stream_wrapper = Pin::into_inner(self);
        // Poll::Ready(Write::flush(stream_wrapper))
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<std::result::Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[derive(Debug)]
pub struct Session {
    pub controller_id: Uuid,
    pub shared_secret: [u8; 32],
}

/// The most plaintext one frame carries.
const FRAME: usize = 1024;
/// A frame's length prefix and authentication tag.
const LENGTH: usize = 2;
const TAG: usize = 16;

/// A session's keys, derived once, and the frame counters that are their nonces.
struct Keys {
    read: [u8; 32],
    write: [u8; 32],
    read_count: u64,
    write_count: u64,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys").finish_non_exhaustive()
    }
}

impl Keys {
    fn new(shared_secret: &[u8; 32]) -> Result<Self> {
        Ok(Keys {
            read: compute_key(shared_secret, b"Control-Write-Encryption-Key")?,
            write: compute_key(shared_secret, b"Control-Read-Encryption-Key")?,
            read_count: 0,
            write_count: 0,
        })
    }

    /// Decrypts one whole frame: length, ciphertext, tag.
    fn open(&mut self, frame: &[u8]) -> io::Result<Vec<u8>> {
        let (length, rest) = frame.split_at(LENGTH);
        let (data, tag) = rest.split_at(rest.len() - TAG);
        let mut plain = data.to_vec();
        ChaCha20Poly1305::new(GenericArray::from_slice(&self.read))
            .decrypt_in_place_detached(&nonce(self.read_count), length, &mut plain, Tag::from_slice(tag))
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "a frame failed its check"))?;
        self.read_count += 1;
        Ok(plain)
    }

    /// Appends `plain` to `out` as encrypted frames.
    fn seal(&mut self, plain: &[u8], out: &mut BytesMut) -> io::Result<()> {
        for chunk in plain.chunks(FRAME) {
            let length = (chunk.len() as u16).to_le_bytes();
            let mut data = chunk.to_vec();
            let tag = ChaCha20Poly1305::new(GenericArray::from_slice(&self.write))
                .encrypt_in_place_detached(&nonce(self.write_count), &length, &mut data)
                .map_err(|_| io::Error::new(ErrorKind::Other, "couldn't encrypt a frame"))?;
            self.write_count += 1;
            out.extend_from_slice(&length);
            out.extend_from_slice(&data);
            out.extend_from_slice(&tag);
        }
        Ok(())
    }
}

fn nonce(count: u64) -> Nonce {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(&count.to_le_bytes());
    *Nonce::from_slice(&nonce)
}

/// Takes the next whole frame off `received`, however the bytes arrived:
/// part of a frame, or several at once.
fn next_frame(received: &mut BytesMut) -> io::Result<Option<BytesMut>> {
    if received.len() < LENGTH {
        return Ok(None);
    }
    let length = u16::from_le_bytes([received[0], received[1]]) as usize;
    if length > FRAME {
        return Err(io::Error::new(ErrorKind::InvalidData, "a frame longer than allowed"));
    }
    let whole = LENGTH + length + TAG;
    Ok((received.len() >= whole).then(|| received.split_to(whole)))
}

/// A controller's TCP connection: plaintext until pair-verify hands over a
/// session, encrypted frames after. Bridges the socket and hyper's channels.
#[derive(Debug)]
pub struct EncryptedStream {
    stream: TcpStream,
    incoming_sender: UnboundedSender<Vec<u8>>,
    outgoing_receiver: UnboundedReceiver<Vec<u8>>,
    incoming_waker: Arc<Mutex<Option<Waker>>>,
    outgoing_waker: Arc<Mutex<Option<Waker>>>,
    session_receiver: oneshot::Receiver<Session>,
    pub controller_id: Arc<RwLock<Option<Uuid>>>,
    keys: Option<Keys>,
    /// Bytes from the socket that are not yet a whole frame.
    received: BytesMut,
    /// Decrypted bytes not yet read.
    plain: BytesMut,
    /// Bytes the socket has not yet taken.
    unsent: BytesMut,
}

impl EncryptedStream {
    pub fn new(
        stream: TcpStream,
    ) -> (
        EncryptedStream,
        UnboundedReceiver<Vec<u8>>,
        UnboundedSender<Vec<u8>>,
        oneshot::Sender<Session>,
        Arc<Mutex<Option<Waker>>>,
        Arc<Mutex<Option<Waker>>>,
    ) {
        let (sender, receiver) = oneshot::channel();
        let (incoming_sender, incoming_receiver) = mpsc::unbounded();
        let (outgoing_sender, outgoing_receiver) = mpsc::unbounded();
        let incoming_waker = Arc::new(Mutex::new(None));
        let outgoing_waker = Arc::new(Mutex::new(None));

        (
            EncryptedStream {
                stream,
                incoming_sender,
                outgoing_receiver,
                incoming_waker: incoming_waker.clone(),
                outgoing_waker: outgoing_waker.clone(),
                session_receiver: receiver,
                controller_id: Arc::new(RwLock::new(None)),
                keys: None,
                received: BytesMut::with_capacity(2 * (LENGTH + FRAME + TAG)),
                plain: BytesMut::with_capacity(FRAME),
                unsent: BytesMut::new(),
            },
            incoming_receiver,
            outgoing_sender,
            sender,
            incoming_waker,
            outgoing_waker,
        )
    }

    /// Takes the session once pair-verify has made one. Only on reading, so
    /// pair-verify's own last reply still goes out in plaintext.
    fn start_session(&mut self) -> io::Result<()> {
        if self.keys.is_none() {
            if let Ok(Some(session)) = self.session_receiver.try_recv() {
                *self.controller_id.write().expect("setting controller_id") = Some(session.controller_id);
                self.keys = Some(
                    Keys::new(&session.shared_secret)
                        .map_err(|_| io::Error::new(ErrorKind::Other, "couldn't derive the session keys"))?,
                );
            }
        }
        Ok(())
    }

    /// Queues `data` for the socket, as frames once there is a session.
    fn queue(&mut self, data: &[u8]) -> io::Result<()> {
        match self.keys.as_mut() {
            Some(keys) => keys.seal(data, &mut self.unsent),
            None => {
                self.unsent.extend_from_slice(data);
                Ok(())
            },
        }
    }

    /// Gives the socket what it will take of the queue.
    fn poll_send(&mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        while !self.unsent.is_empty() {
            let sent = ready!(Pin::new(&mut self.stream).poll_write(cx, &self.unsent))?;
            if sent == 0 {
                return Poll::Ready(Err(ErrorKind::WriteZero.into()));
            }
            self.unsent.advance(sent);
        }
        Poll::Ready(Ok(()))
    }

    fn poll_outgoing(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::result::Result<(), io::Error>> {
        let encrypted_stream = Pin::into_inner(self);
        loop {
            // Finish what is queued before taking more; the socket wakes us.
            ready!(encrypted_stream.poll_send(cx))?;
            match Stream::poll_next(Pin::new(&mut encrypted_stream.outgoing_receiver), cx) {
                Poll::Pending => {
                    *encrypted_stream.outgoing_waker.lock().expect("setting outgoing_waker") = Some(cx.waker().clone());
                    return Poll::Pending;
                },
                Poll::Ready(Some(data)) => {
                    debug!("writing {} Bytes to outgoing TCP stream", data.len());
                    encrypted_stream.queue(&data)?;
                },
                Poll::Ready(None) => {
                    debug!("outgoing TCP stream ended");
                    return Poll::Ready(Ok(()));
                },
            }
        }
    }

    fn poll_incoming(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::result::Result<(), io::Error>> {
        let encrypted_stream = Pin::into_inner(self);

        let mut data_inner = [0; 1536];
        let mut data = ReadBuf::new(&mut data_inner);

        loop {
            match AsyncRead::poll_read(Pin::new(encrypted_stream), cx, &mut data) {
                Poll::Pending => {
                    *encrypted_stream.incoming_waker.lock().expect("setting incoming_waker") = Some(cx.waker().clone());
                    return Poll::Pending;
                },
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {
                    if data.filled().is_empty() {
                        return Poll::Ready(Ok(()));
                    }
                    encrypted_stream
                        .incoming_sender
                        .unbounded_send(data.filled().to_vec())
                        .map_err(|_| io::Error::new(ErrorKind::Other, "couldn't send incoming data"))?;
                    data.clear();
                },
            }
        }
    }
}

impl Future for EncryptedStream {
    type Output = std::result::Result<(), io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let encrypted_stream = Pin::into_inner(self);
        if let Poll::Ready(Err(e)) = EncryptedStream::poll_outgoing(Pin::new(encrypted_stream), cx) {
            error!("error writing to outgoing stream: {}", e);
            return Poll::Ready(Err(e));
        }
        EncryptedStream::poll_incoming(Pin::new(encrypted_stream), cx)
    }
}

impl AsyncRead for EncryptedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::result::Result<(), io::Error>> {
        let encrypted_stream = Pin::into_inner(self);
        encrypted_stream.start_session()?;
        let Some(keys) = encrypted_stream.keys.as_mut() else {
            return AsyncRead::poll_read(Pin::new(&mut encrypted_stream.stream), cx, buf);
        };
        loop {
            if !encrypted_stream.plain.is_empty() {
                let length = min(buf.remaining(), encrypted_stream.plain.len());
                buf.put_slice(&encrypted_stream.plain.split_to(length));
                return Poll::Ready(Ok(()));
            }
            if let Some(frame) = next_frame(&mut encrypted_stream.received)? {
                let plain = keys.open(&frame)?;
                encrypted_stream.plain.extend_from_slice(&plain);
                continue;
            }
            let mut chunk = [0; 4096];
            let mut read = ReadBuf::new(&mut chunk);
            ready!(AsyncRead::poll_read(Pin::new(&mut encrypted_stream.stream), cx, &mut read))?;
            if read.filled().is_empty() {
                // The controller hung up, maybe mid-frame.
                return Poll::Ready(Ok(()));
            }
            encrypted_stream.received.extend_from_slice(read.filled());
        }
    }
}

impl AsyncWrite for EncryptedStream {
    /// Queues everything and sends what the socket takes now; the rest goes
    /// out with the next write or flush.
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<std::result::Result<usize, Error>> {
        let encrypted_stream = Pin::into_inner(self);
        encrypted_stream.queue(buf)?;
        if let Poll::Ready(Err(e)) = encrypted_stream.poll_send(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::result::Result<(), Error>> {
        let encrypted_stream = Pin::into_inner(self);
        ready!(encrypted_stream.poll_send(cx))?;
        AsyncWrite::poll_flush(Pin::new(&mut encrypted_stream.stream), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::result::Result<(), Error>> {
        let encrypted_stream = Pin::into_inner(self);
        ready!(encrypted_stream.poll_send(cx))?;
        AsyncWrite::poll_shutdown(Pin::new(&mut encrypted_stream.stream), cx)
    }
}

fn compute_key(shared_secret: &[u8; 32], info: &[u8]) -> Result<[u8; 32]> {
    super::hkdf_extract_and_expand(b"Control-Salt", shared_secret, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_open_however_the_bytes_arrive() {
        let key = [7; 32];
        let mut sender = Keys {
            read: key,
            write: key,
            read_count: 0,
            write_count: 0,
        };
        let mut receiver = Keys {
            read: key,
            write: key,
            read_count: 0,
            write_count: 0,
        };
        let message: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let mut wire = BytesMut::new();
        sender.seal(&message, &mut wire).unwrap();
        sender.seal(b"second", &mut wire).unwrap();
        assert_eq!(wire.len(), 4 * (LENGTH + TAG) + 2500 + 6);

        // Three bytes at a time: frames still come out whole, and in order.
        let mut received = BytesMut::new();
        let mut opened = Vec::new();
        for piece in wire.chunks(3) {
            received.extend_from_slice(piece);
            while let Some(frame) = next_frame(&mut received).unwrap() {
                opened.extend(receiver.open(&frame).unwrap());
            }
        }
        assert_eq!(opened, [message, b"second".to_vec()].concat());

        // All at once: every frame comes out of one read.
        let mut receiver = Keys {
            read: key,
            write: key,
            read_count: 0,
            write_count: 0,
        };
        let mut received = wire.clone();
        let mut frames = 0;
        while let Some(frame) = next_frame(&mut received).unwrap() {
            receiver.open(&frame).unwrap();
            frames += 1;
        }
        assert_eq!((frames, received.len()), (4, 0));

        // A changed byte, or a frame out of order, fails its check.
        let mut tampered = wire.clone();
        tampered[5] ^= 1;
        let frame = next_frame(&mut tampered).unwrap().unwrap();
        assert!(Keys { read: key, write: key, read_count: 0, write_count: 0 }.open(&frame).is_err());
    }
}
