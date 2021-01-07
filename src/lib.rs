//! Wrapper types that prepends data when reading or writing with [`AsyncRead`] or [`AsyncWrite`].
use bytes::{Buf, BytesMut};
use futures::ready;
use std::cmp;
use std::io::Result;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

macro_rules! delegate_write {
    () => {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }
    };
}

macro_rules! delegate_flush {
    () => {
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
    };
}

macro_rules! delegate_shutdown {
    () => {
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    };
}

macro_rules! delegate_read {
    ($type:ident) => {
        impl<R: AsyncRead + Unpin> AsyncRead for $type<R> {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<Result<()>> {
                Pin::new(&mut self.inner).poll_read(cx, buf)
            }
        }
    };
}

macro_rules! delegate_write_all {
    ($type:ident) => {
        impl<W: AsyncWrite + Unpin> AsyncWrite for $type<W> {
            delegate_write!();
            delegate_flush!();
            delegate_shutdown!();
        }
    };
}
/// A wrapper that produces prepended data before forwarding all read operations.
pub struct PrependReader<R> {
    inner: R,
    prepend: Option<BytesMut>,
}

impl<R: AsyncRead> PrependReader<R> {
    /// Creates a new reader
    pub fn new<P: Into<BytesMut>>(inner: R, prepend: P) -> Self {
        let prepend = prepend.into();
        let prepend = if prepend.is_empty() {
            None
        } else {
            Some(prepend)
        };
        PrependReader { inner, prepend }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for PrependReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        if let Some(ref mut prepend_buf) = self.prepend {
            let n = cmp::min(buf.remaining(), prepend_buf.len());
            buf.put_slice(&prepend_buf[..n]);
            prepend_buf.advance(n);
            if prepend_buf.is_empty() {
                // All consumed
                self.prepend = None;
            }
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }
}

delegate_write_all!(PrependReader);

/// A wrapper that writes prepended data to underlying stream before forwarding all writing operations.
pub struct PrependWriter<W> {
    inner: W,
    prepend: Option<BytesMut>,
    len_before_concat: Option<usize>,
    written: usize,
}

impl<W: AsyncWrite> PrependWriter<W> {
    /// Creates a new writer
    pub fn new<P: Into<BytesMut>>(inner: W, prepend: P) -> Self {
        let prepend = prepend.into();
        let prepend = if prepend.is_empty() {
            None
        } else {
            Some(prepend)
        };
        PrependWriter {
            inner,
            prepend,
            len_before_concat: None,
            written: 0,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for PrependWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        let me = &mut *self;
        if let Some(ref mut prepend_buf) = me.prepend {
            // We need to write at least one byte in the actual input buffer
            loop {
                if me.len_before_concat.is_none() {
                    me.len_before_concat = Some(prepend_buf.len());
                    prepend_buf.extend_from_slice(buf); // Append our buffer with input data
                }

                let n = ready!(Pin::new(&mut me.inner).poll_write(cx, &prepend_buf))?;
                me.written += n;

                if me.written > me.len_before_concat.unwrap() {
                    me.prepend = None;
                    return Poll::Ready(Ok(me.written - me.len_before_concat.unwrap()));
                }
            }
        } else {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }
    }
    delegate_flush!();
    delegate_shutdown!();
}

delegate_read!(PrependWriter);

#[cfg(test)]
mod test {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_test::io::Builder;

    #[tokio::test]
    async fn reader() {
        let mock = Builder::new().read(b"AAA").build();
        let prepend = b"BBB";
        let mut reader = PrependReader::new(mock, &prepend[..]);
        let mut buf = vec![];
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"BBBAAA");
    }

    #[tokio::test]
    async fn writer() {
        let prepend = b"AAA";
        let mock = Builder::new().write(prepend).write(b"BBB").build();
        let mut writer = PrependWriter::new(mock, &prepend[..]);
        writer.write_all(b"BBB").await.unwrap();
    }
}
