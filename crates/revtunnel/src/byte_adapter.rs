//! `TunnelIo` — wraps the outer bidi stream as `AsyncRead + AsyncWrite`.
//!
//! Generic over the inbound stream type so unit tests can drive it with
//! `tokio_stream::wrappers::ReceiverStream`; production code uses the
//! default `tonic::Streaming<TunnelFrame>`.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use meshmon_protocol::TunnelFrame;
use pin_project_lite::pin_project;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;
use tonic::Status;

pin_project! {
    /// Adapter that exposes a bidi `TunnelFrame` stream as
    /// `AsyncRead + AsyncWrite + Unpin` for yamux.
    ///
    /// The channel capacity for the outgoing `mpsc` should be **16** to cap
    /// worst-case buffering at ~1 MiB (16 × 64 KiB yamux frames).
    ///
    /// Generic parameter `S` defaults to `tonic::Streaming<TunnelFrame>` for
    /// production use; tests substitute `tokio_stream::wrappers::ReceiverStream`.
    pub struct TunnelIo<S = tonic::Streaming<TunnelFrame>> {
        #[pin]
        incoming: S,
        // PollSender is Unpin; no #[pin] needed.
        outgoing: PollSender<Result<TunnelFrame, Status>>,
        read_buf: BytesMut,
        pending_write: Option<TunnelFrame>,
        read_eof: bool,
    }
}

impl<S> TunnelIo<S>
where
    S: Stream<Item = Result<TunnelFrame, Status>> + Unpin,
{
    /// Build a new adapter.
    ///
    /// * `incoming` – the server-streaming half delivering frames into this
    ///   end (e.g. `tonic::Streaming` or a test `ReceiverStream`).
    /// * `outgoing` – sender that pushes frames back to the remote end.
    ///   Use a capacity of **16** for production.
    pub fn new(incoming: S, outgoing: mpsc::Sender<Result<TunnelFrame, Status>>) -> Self {
        Self {
            incoming,
            outgoing: PollSender::new(outgoing),
            read_buf: BytesMut::with_capacity(64 * 1024),
            pending_write: None,
            read_eof: false,
        }
    }
}

impl<S> AsyncRead for TunnelIo<S>
where
    S: Stream<Item = Result<TunnelFrame, Status>> + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.project();

        // 1. Drain the partial-read buffer first (invariant 2).
        if !me.read_buf.is_empty() {
            let n = std::cmp::min(buf.remaining(), me.read_buf.len());
            let chunk = me.read_buf.split_to(n);
            buf.put_slice(&chunk);
            return Poll::Ready(Ok(()));
        }

        // 2. EOF already signalled and buffer drained (invariant 3).
        if *me.read_eof {
            return Poll::Ready(Ok(()));
        }

        // 3. Pull the next frame (invariant 1: no lock across .await).
        match me.incoming.poll_next(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let data: Bytes = frame.data;
                let n = std::cmp::min(buf.remaining(), data.len());
                buf.put_slice(&data[..n]);
                if n < data.len() {
                    me.read_buf.extend_from_slice(&data[n..]);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Some(Err(status))) => {
                // Invariant 4: map Status → io::Error with kind Other.
                Poll::Ready(Err(io::Error::other(format!(
                    "tunnel recv status: {status}"
                ))))
            }
            Poll::Ready(None) => {
                // Invariant 3: EOF — zero bytes filled, mark done.
                *me.read_eof = true;
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for TunnelIo<S>
where
    S: Stream<Item = Result<TunnelFrame, Status>> + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.project();

        // 1. Flush any held-back frame first (invariant 5).
        //
        // The pending frame was built from a previous call's `buf` and the
        // caller (e.g. `write_all`) will supply the same `buf` again on this
        // call.  Once we successfully send the pending frame we must return
        // `Ready(Ok(len))` — NOT fall through to build and send a second
        // copy of the same data.
        if let Some(frame) = me.pending_write.take() {
            let len = frame.data.len();
            match me.outgoing.poll_reserve(cx) {
                Poll::Ready(Ok(())) => {
                    me.outgoing
                        .send_item(Ok(frame))
                        .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
                    // The bytes for this `buf` are now in-flight; report success.
                    return Poll::Ready(Ok(len));
                }
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
                }
                Poll::Pending => {
                    // Put it back so it will be retried next time.
                    *me.pending_write = Some(frame);
                    return Poll::Pending;
                }
            }
        }

        // 2. Build frame for this write.
        let frame = TunnelFrame {
            data: Bytes::copy_from_slice(buf),
        };

        // 3. Try to send (invariant 5: never drop frames silently).
        match me.outgoing.poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                let len = buf.len();
                me.outgoing
                    .send_item(Ok(frame))
                    .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
                Poll::Ready(Ok(len))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe))),
            Poll::Pending => {
                // Stash frame; will be retried on the next poll.
                *me.pending_write = Some(frame);
                Poll::Pending
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.project();
        if let Some(frame) = me.pending_write.take() {
            match me.outgoing.poll_reserve(cx) {
                Poll::Ready(Ok(())) => {
                    me.outgoing
                        .send_item(Ok(frame))
                        .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
                }
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
                }
                Poll::Pending => {
                    *me.pending_write = Some(frame);
                    return Poll::Pending;
                }
            }
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Drain any pending frame before closing.
        futures_util::ready!(self.as_mut().poll_flush(cx))?;
        let me = self.project();
        me.outgoing.close();
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use meshmon_protocol::TunnelFrame;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    // Type aliases to keep helper signatures below clippy::type_complexity threshold.
    type Frame = Result<TunnelFrame, Status>;
    type TestIo = TunnelIo<ReceiverStream<Frame>>;

    fn build() -> (
        TestIo,
        mpsc::Sender<Frame>,   // push "received" frames in
        mpsc::Receiver<Frame>, // drain "sent" frames out
    ) {
        let (incoming_tx, incoming_rx) = mpsc::channel::<Frame>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Frame>(16);
        let io = TunnelIo::new(ReceiverStream::new(incoming_rx), outgoing_tx);
        (io, incoming_tx, outgoing_rx)
    }

    fn build_with_write_capacity(
        cap: usize,
    ) -> (TestIo, mpsc::Sender<Frame>, mpsc::Receiver<Frame>) {
        let (incoming_tx, incoming_rx) = mpsc::channel::<Frame>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Frame>(cap);
        let io = TunnelIo::new(ReceiverStream::new(incoming_rx), outgoing_tx);
        (io, incoming_tx, outgoing_rx)
    }

    #[tokio::test]
    async fn reads_small_frame_in_one_go() {
        let (mut io, incoming_tx, _out) = build();
        incoming_tx
            .send(Ok(TunnelFrame {
                data: Bytes::from_static(b"hello"),
            }))
            .await
            .unwrap();
        let mut buf = vec![0u8; 8];
        let n = io.read(&mut buf).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");
    }

    #[tokio::test]
    async fn reads_large_frame_in_multiple_reads() {
        let (mut io, incoming_tx, _out) = build();
        incoming_tx
            .send(Ok(TunnelFrame {
                data: Bytes::from_static(b"0123456789"),
            }))
            .await
            .unwrap();

        let mut buf = [0u8; 4];
        let n1 = io.read(&mut buf).await.unwrap();
        assert_eq!(n1, 4);
        assert_eq!(&buf, b"0123");

        let n2 = io.read(&mut buf).await.unwrap();
        assert_eq!(n2, 4);
        assert_eq!(&buf, b"4567");

        let mut tail = [0u8; 4];
        let n3 = io.read(&mut tail).await.unwrap();
        assert_eq!(n3, 2);
        assert_eq!(&tail[..2], b"89");
    }

    #[tokio::test]
    async fn maps_end_of_stream_to_eof() {
        let (mut io, incoming_tx, _out) = build();
        incoming_tx
            .send(Ok(TunnelFrame {
                data: Bytes::from_static(b"abc"),
            }))
            .await
            .unwrap();
        drop(incoming_tx);

        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await.unwrap();
        assert_eq!(&buf, b"abc");
    }

    #[tokio::test]
    async fn maps_status_error_to_io_error_other() {
        let (mut io, incoming_tx, _out) = build();
        incoming_tx
            .send(Err(Status::internal("boom")))
            .await
            .unwrap();
        let mut buf = [0u8; 8];
        let err = io.read(&mut buf).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[tokio::test]
    async fn writes_produce_tunnel_frames() {
        let (mut io, _in, mut outgoing_rx) = build();
        io.write_all(b"hello").await.unwrap();
        let frame = outgoing_rx.recv().await.expect("frame").expect("ok");
        assert_eq!(&frame.data[..], b"hello");
    }

    #[tokio::test]
    async fn write_backpressure_when_channel_full() {
        // Capacity 1 so the second write parks until we drain.
        let (io, _in, mut outgoing_rx) = build_with_write_capacity(1);
        let io = std::sync::Arc::new(tokio::sync::Mutex::new(io));

        // First write succeeds immediately.
        {
            let mut guard = io.lock().await;
            guard.write_all(b"first").await.unwrap();
        }

        // Second write parks until a frame is drained.
        let io2 = io.clone();
        let writer = tokio::spawn(async move {
            let mut guard = io2.lock().await;
            guard.write_all(b"second").await.unwrap();
        });

        // Hold briefly to assert the task is still pending.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !writer.is_finished(),
            "second write should still be pending"
        );

        // Drain one frame.
        let first = outgoing_rx.recv().await.expect("first frame").expect("ok");
        assert_eq!(&first.data[..], b"first");

        // Pending writer now unblocks.
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .expect("unblocked in time")
            .expect("task ok");
        let second = outgoing_rx.recv().await.expect("second frame").expect("ok");
        assert_eq!(&second.data[..], b"second");
    }

    #[tokio::test]
    async fn write_after_shutdown_is_broken_pipe() {
        let (mut io, _in, _out) = build();
        io.shutdown().await.unwrap();
        let err = io.write_all(b"after").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
