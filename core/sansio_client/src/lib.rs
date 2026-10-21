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

//! A sans-IO Iggy client.
//!
//! [`Connection`] runs the client side of the Iggy binary protocol: login,
//! session bind and resume, request ids, partition routes, lanes, retries,
//! deadlines, heartbeat and eviction. It never touches a socket, a clock or
//! a thread, and it needs only `core` and `alloc`. So one state machine can
//! serve a blocking `std::net` loop, ESP-IDF, Embassy or tokio.
//!
//! The driver owns everything else: dialing, TLS, reconnect timing, the
//! choice of endpoint, the clock and the random source.
//!
//! # The driver loop
//!
//! `Connection` implements [`sansio::Protocol`]. A driver loops like this:
//!
//! ```text
//! loop {
//!     conn.handle_timeout(clock.now())          // always first
//!     match wait_for(socket, app, conn.poll_timeout()) {
//!         Bytes(chunk)      => conn.handle_read(chunk),
//!         Request(submit)   => conn.handle_write(submit),
//!         Connected         => conn.handle_event(Input::Connected),
//!         Closed            => conn.handle_event(Input::Disconnected),
//!         TimerFired        => {}                // handled at the top
//!     }
//!     while let Some(t) = conn.poll_write()  { socket.write(t.header); socket.write(t.body) }
//!     while let Some(c) = conn.poll_read()   { app.complete(c.handle, c.result) }
//!     while let Some(e) = conn.poll_event()  { if e == CloseTransport { socket.close(); redial() } }
//! }
//! ```
//!
//! `examples/blocking_tcp.rs` is a complete driver over `std::net`.
//!
//! # Rules for the driver
//!
//! - The machine knows the time only from `handle_timeout`. Call it with
//!   the current time before any other input in a loop turn.
//! - Write each [`Transmit`] whole, header then body, in the order that
//!   `poll_write` returns them.
//! - After `Input::Disconnected` or `Output::CloseTransport`, the bytes of
//!   the old connection are gone. Dial again and send `Input::Connected`.
//!   A bound session resumes by itself.
//! - Pass a cryptographic [`Entropy`]. The bind secret proves ownership of
//!   the session.
//!
//! # Results that need care
//!
//! - [`ClientError::OperationUncertain`]: a write can have taken effect,
//!   and its lane is blocked. Send [`Command::Resolve`] to learn the
//!   outcome, or call [`Connection::abandon`].
//! - [`Output::SessionReset`]: the server lost the session. The machine
//!   keeps no credentials, so the application signs in again.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "std"))]
extern crate std;

mod command;
mod config;
mod connection;
mod entropy;
mod error;
mod framer;
mod header;
mod lane;
mod retry;
mod route;
mod session;
mod time;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use command::{Command, Message, PolledMessages, Reply};
pub use config::{Config, DEFAULT_SDK_NAME};
pub use connection::{Completion, Connection, Input, Output, RequestHandle, Submit, Transmit};
pub use entropy::Entropy;
pub use error::{ClientError, codes};
pub use time::Instant;

pub use iggy_binary_protocol;
pub use sansio::Protocol;
pub use secrecy::SecretString;
