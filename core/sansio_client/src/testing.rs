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

//! Frames and a random source for the tests, modeled on `test_support` in
//! `core/sdk/src/links/mod.rs`.

use crate::entropy::Entropy;
use alloc::vec::Vec;
use core::mem::offset_of;
use iggy_binary_protocol::consensus::{
    Command, EvictionHeader, EvictionReason, HEADER_SIZE, Operation, ReplyHeader, SIZE_FIELD_OFFSET,
};

pub const CLIENT: u128 = 7;
pub const COMMIT: u64 = 20;

/// A predictable random source: each byte counts up.
#[derive(Debug, Default)]
pub struct Counter(u8);

impl Entropy for Counter {
    fn fill(&mut self, buf: &mut [u8]) {
        for byte in buf {
            self.0 = self.0.wrapping_add(1);
            *byte = self.0;
        }
    }
}

/// A non-replicated reply to `request` with `body`.
pub fn reply(request: u64, body: &[u8]) -> Vec<u8> {
    frame(
        ReplyHeader {
            command: Command::Reply,
            operation: Operation::NonReplicated,
            client: CLIENT,
            request,
            ..Default::default()
        },
        body,
    )
}

/// A reply of `operation` that shows the metadata commit [`COMMIT`].
pub fn reply_header(request: u64, operation: Operation, body: &[u8]) -> Vec<u8> {
    frame(
        ReplyHeader {
            command: Command::Reply,
            operation,
            client: CLIENT,
            request,
            commit: COMMIT,
            ..Default::default()
        },
        body,
    )
}

/// A reply that refuses `request` with the status `code`.
pub fn refusal(request: u64, code: u32) -> Vec<u8> {
    frame(
        ReplyHeader {
            command: Command::Reply,
            operation: Operation::NonReplicated,
            client: CLIENT,
            request,
            status: code,
            ..Default::default()
        },
        &[],
    )
}

pub fn frame(mut header: ReplyHeader, body: &[u8]) -> Vec<u8> {
    header.size = u32::try_from(HEADER_SIZE + body.len()).unwrap();
    let mut frame = bytemuck::bytes_of(&header).to_vec();
    frame.extend_from_slice(body);
    frame
}

/// An eviction header, written field by field, because `EvictionHeader`
/// has no `Default`.
pub fn eviction(reason: EvictionReason) -> [u8; HEADER_SIZE] {
    let mut bytes = [0; HEADER_SIZE];
    let size = u32::try_from(HEADER_SIZE).unwrap();
    bytes[SIZE_FIELD_OFFSET..SIZE_FIELD_OFFSET + 4].copy_from_slice(&size.to_le_bytes());
    bytes[offset_of!(EvictionHeader, command)] = Command::Eviction as u8;
    let client = offset_of!(EvictionHeader, client);
    bytes[client..client + 16].copy_from_slice(&CLIENT.to_le_bytes());
    bytes[offset_of!(EvictionHeader, reason)] = reason as u8;
    bytes
}
