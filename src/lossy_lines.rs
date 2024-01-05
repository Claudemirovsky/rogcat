// Copyright (c) 2024 Tokio Contributors
// see: https://docs.rs/tokio-util/0.7.10/src/tokio_util/codec/lines_codec.rs.html

use bytes::{Buf, BufMut, BytesMut};
use futures::{
    task::{Context, Poll},
    FutureExt, Stream,
};
use std::{cmp, pin::Pin, usize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead};
use tokio_util::codec::{Decoder, Encoder, LinesCodecError};

/// Combinator created by the top-level `lossy_lines` method which is a stream over
/// the lines of text on an I/O object.
#[derive(Debug)]
pub struct LossyLines<A> {
    io: Pin<Box<A>>,
    buffer: Vec<u8>,
}

/// Creates a new stream from the I/O object given representing the lines of
/// input that are found on `A`.
///
/// This method takes an asynchronous I/O object, `a`, and returns a `Stream` of
/// lines that the object contains. The returned stream will reach its end once
/// `a` reaches EOF.
pub fn lossy_lines<A>(a: A) -> LossyLines<A>
where
    A: AsyncRead + AsyncBufRead,
{
    LossyLines {
        io: Box::pin(a),
        buffer: Vec::new(),
    }
}

impl<A> Stream for LossyLines<A>
where
    A: AsyncRead + AsyncBufRead,
{
    type Item = String;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let buffer = this.buffer.as_mut();
        let read = Box::pin(this.io.read_until(b'\n', buffer)).poll_unpin(cx);
        let n = match read {
            Poll::Ready(Ok(t)) => t,
            Poll::Ready(Err(ref e)) if e.kind() == ::std::io::ErrorKind::WouldBlock => {
                return Poll::Pending;
            }
            Poll::Ready(Err(_)) => return Poll::Ready(None),
            Poll::Pending => return Poll::Pending,
        };
        if n == 0 && buffer.is_empty() {
            Poll::Ready(None)
        } else {
            // Strip all \r\n occurences because on Windows "adb logcat" ends lines with "\r\r\n"
            while buffer.ends_with(&[b'\r']) || buffer.ends_with(&[b'\n']) {
                buffer.pop();
            }
            let line = String::from_utf8_lossy(buffer).into();
            buffer.clear();
            Poll::Ready(Some(line))
        }
    }
}

pub struct LossyLinesCodec {
    next_index: usize,
    max_length: usize,
    is_discarding: bool,
}

impl LossyLinesCodec {
    pub fn new() -> LossyLinesCodec {
        LossyLinesCodec {
            next_index: 0,
            max_length: usize::MAX,
            is_discarding: false,
        }
    }
}

fn without_carriage_return(s: &[u8]) -> &[u8] {
    if let Some(&b'\r') = s.last() {
        &s[..s.len() - 1]
    } else {
        s
    }
}

impl Decoder for LossyLinesCodec {
    type Item = String;
    type Error = LinesCodecError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<String>, LinesCodecError> {
        loop {
            // Determine how far into the buffer we'll search for a newline. If
            // there's no max_length set, we'll read to the end of the buffer.
            let read_to = cmp::min(self.max_length.saturating_add(1), buf.len());

            let newline_offset = buf[self.next_index..read_to]
                .iter()
                .position(|b| *b == b'\n');

            match (self.is_discarding, newline_offset) {
                (true, Some(offset)) => {
                    // If we found a newline, discard up to that offset and
                    // then stop discarding. On the next iteration, we'll try
                    // to read a line normally.
                    buf.advance(offset + self.next_index + 1);
                    self.is_discarding = false;
                    self.next_index = 0;
                }
                (true, None) => {
                    // Otherwise, we didn't find a newline, so we'll discard
                    // everything we read. On the next iteration, we'll continue
                    // discarding up to max_len bytes unless we find a newline.
                    buf.advance(read_to);
                    self.next_index = 0;
                    if buf.is_empty() {
                        return Ok(None);
                    }
                }
                (false, Some(offset)) => {
                    // Found a line!
                    let newline_index = offset + self.next_index;
                    self.next_index = 0;
                    let line = buf.split_to(newline_index + 1);
                    let line = &line[..line.len() - 1];
                    let line = without_carriage_return(line);
                    let line = String::from_utf8_lossy(line);
                    return Ok(Some(line.to_string()));
                }
                (false, None) if buf.len() > self.max_length => {
                    // Reached the maximum length without finding a
                    // newline, return an error and start discarding on the
                    // next call.
                    self.is_discarding = true;
                    return Err(LinesCodecError::MaxLineLengthExceeded);
                }
                (false, None) => {
                    // We didn't find a line or reach the length limit, so the next
                    // call will resume searching at the current offset.
                    self.next_index = read_to;
                    return Ok(None);
                }
            }
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<String>, LinesCodecError> {
        Ok(match self.decode(buf)? {
            Some(frame) => Some(frame),
            None => {
                // No terminating newline - return remaining data, if any
                if buf.is_empty() || buf == &b"\r"[..] {
                    None
                } else {
                    let line = buf.split_to(buf.len());
                    let line = without_carriage_return(&line);
                    let line = String::from_utf8_lossy(line);
                    self.next_index = 0;
                    Some(line.to_string())
                }
            }
        })
    }
}

impl<T> Encoder<T> for LossyLinesCodec
where
    T: AsRef<str>,
{
    type Error = LinesCodecError;

    fn encode(&mut self, line: T, buf: &mut BytesMut) -> Result<(), Self::Error> {
        let line = line.as_ref();
        buf.reserve(line.len() + 1);
        buf.put(line.as_bytes());
        buf.put_u8(b'\n');
        Ok(())
    }
}

impl Default for LossyLinesCodec {
    fn default() -> Self {
        Self::new()
    }
}
