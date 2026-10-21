// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

//! Splits a byte stream into frames, as `deliver_buffered` in
//! `core/sdk/src/links/mux.rs` does. The chunks can have any size: one byte,
//! one frame, or several frames at once.

use crate::header::frame_size;
use bytes::{Bytes, BytesMut};
use iggy_binary_protocol::consensus::HEADER_SIZE;

/// One whole frame from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFrame {
    pub header: [u8; HEADER_SIZE],
    pub body: Bytes,
}

/// The stream holds a frame that the machine must not read. The connection
/// can no longer be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadFrame;

#[derive(Debug, Default)]
pub struct Framer {
    buffer: BytesMut,
}

impl Framer {
    pub fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Cuts the next whole frame off the front of the buffer. A size below
    /// the header size or above `max_frame_size` is a bad frame. The body
    /// shares the buffer, so it is not copied.
    pub fn next_frame(&mut self, max_frame_size: usize) -> Result<Option<InFrame>, BadFrame> {
        let Some(&header) = self.buffer.first_chunk::<HEADER_SIZE>() else {
            return Ok(None);
        };
        let size = frame_size(&header)
            .filter(|size| *size <= max_frame_size)
            .ok_or(BadFrame)?;
        if self.buffer.len() < size {
            // Ask for room for the whole frame once, not for each chunk.
            self.buffer.reserve(size - self.buffer.len());
            return Ok(None);
        }
        let body = self.buffer.split_to(size).freeze().slice(HEADER_SIZE..);
        Ok(Some(InFrame { header, body }))
    }

    pub fn clear(&mut self) {
        self.buffer = BytesMut::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::reply;
    use alloc::vec::Vec;

    #[test]
    fn frames_split_into_single_bytes_come_out_whole() {
        let mut stream = reply(1, b"first");
        stream.extend(reply(2, b""));
        stream.extend(reply(3, b"third"));
        let mut framer = Framer::default();
        let mut received = Vec::new();
        for byte in stream {
            framer.push(&[byte]);
            while let Some(frame) = framer.next_frame(1024).unwrap() {
                received.push(frame);
            }
        }
        let bodies: Vec<&[u8]> = received.iter().map(|frame| &frame.body[..]).collect();
        assert_eq!(bodies, [&b"first"[..], b"", b"third"]);
    }

    #[test]
    fn a_frame_above_the_limit_is_bad() {
        let mut framer = Framer::default();
        framer.push(&reply(1, &[0; 100]));
        assert_eq!(framer.next_frame(HEADER_SIZE + 99), Err(BadFrame));
    }

    #[test]
    fn a_size_below_the_header_is_bad() {
        let mut frame = reply(1, b"");
        frame[48..52].copy_from_slice(&10_u32.to_le_bytes());
        let mut framer = Framer::default();
        framer.push(&frame);
        assert_eq!(framer.next_frame(1024), Err(BadFrame));
    }
}
