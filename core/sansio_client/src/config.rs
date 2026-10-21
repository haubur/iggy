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

//! The settings of one connection.

use alloc::string::String;
use core::time::Duration;

/// The name that the machine reports in `ClientVersionInfo` by default.
pub const DEFAULT_SDK_NAME: &str = "rust-sansio";

/// The settings of one [`Connection`](crate::Connection).
///
/// The defaults follow the Rust SDK where it has a value, and stay small
/// where the SDK sizes for a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The SDK name in `ClientVersionInfo`, 1 to 255 bytes.
    pub sdk_name: String,
    /// The SDK version in `ClientVersionInfo`, 1 to 255 bytes.
    pub sdk_version: String,
    /// The longest silence on the connection before the machine sends a
    /// ping. The server ends the session of a connection that stays silent
    /// for 1.2 times its own heartbeat interval (36 s by default).
    pub heartbeat_interval: Duration,
    /// The longest that one request waits, from its first attempt to its
    /// last reply, unless the request names its own timeout. The same
    /// limit bounds a bind and a heartbeat ping.
    pub request_timeout: Duration,
    /// The most frames on the wire at once. It matches the queue that the
    /// server keeps for one connection.
    pub max_in_flight: usize,
    /// The most requests that wait to go on the wire. A new request beyond
    /// it fails with [`ClientError::QueueFull`](crate::ClientError::QueueFull).
    pub max_queued: usize,
    /// The largest reply frame, header included. A larger one closes the
    /// connection, so that a small device does not run out of memory.
    pub max_frame_size: usize,
    /// The most partition routes that the machine keeps. A new route beyond
    /// it evicts the oldest lookup.
    pub max_cached_routes: usize,
    /// The most outcomes of resolved operations that the machine keeps for
    /// a repeated resolve.
    pub max_proved_outcomes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sdk_name: String::from(DEFAULT_SDK_NAME),
            sdk_version: String::from(env!("CARGO_PKG_VERSION")),
            heartbeat_interval: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            max_in_flight: 256,
            max_queued: 1024,
            max_frame_size: 64 * 1024 * 1024,
            max_cached_routes: 64,
            max_proved_outcomes: 16,
        }
    }
}
