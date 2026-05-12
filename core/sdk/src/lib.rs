/* Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 *
 *   http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

//! # Apache Iggy SDK
//! Apache Iggy is a high-performance, persistent message streaming platform written in Rust, capable of processing millions of messages per second with ultra-low latency.
//! It is part of the [`Incubating Program`] of the [`Apache Software Foundation`] (ASF).
//!
//! The core of Iggy is a persisted append-only log data structure.
//! The Iggy server is build around this data structure and consists of logic to allow consumers and producers to read and write data to and from that log.
//! An overview of how this is implemented can be found on the [`website`]. Deep dives into design decisions and server internals to ensure high-performance standards are outlined in various extensive [`blog posts`].
//!
//! **This library is the Apache Iggy SDK for the Rust programming language.**
//! SDKs for other programming languages can be found in [`core/foreign`] of the root repository on GitHub and respective package managers such as npm, maven, pypi and nuget.
//! The SDK is not concerned with server internals but with the interface the server exposes to the user to trigger administrative operations or to read and write to the append-only log.
//! Specifically they wrap server commands around core functionality that is frequently required to build robust message streaming solutions, such as retry or auto-batching logic to name a few.
//!
//! If you want to start and talk to an Iggy server right away follow the [`Getting Started`] guide from the website.
//! To follow up with more advanced use-cases have a look at the [`examples`].
//!
//! The following section will go into details on the different APIs the Rust SDK provides.
//! For any question really feel free to join our [`discord`] where maintainers and contributers are happy to help.
//!
//! ## Server Command Overview
//! In its essence the Rust SDK wraps core feature of meassage streaming applications around Iggy server commands.
//! A comprehensive overview of commands can be found in the [`schema spec`] on the website or checking the [`server command enum`] within the source code.
//! There are a couple *domains* of server commands:
//! * System
//! * Users
//! * Personal Access Token
//! * Streams
//! * Topics
//! * Partitions
//! * Segments
//! * Consumer Groups
//!
//! ## Communicating with the server
//! Users can trigger commands when connected to the server. Since connections can be established using TCP, QUIC, HTTP and WebSocket protocols,
//! [`iggy_binary_protocl`] implements a common language/ schema that is shared between the server and the SDK for communication.
//!
//!
//! - What is the protocol to trigger these commands? Why does Iggy have a unique protocol?
//!   - The binary protocol implements logic to not copy incoming message payloads multiple times until write to disk (zero-copy).
//!
//! Is there a better way to structure the Rust SDK?
//!
//!
//! ### Low-Level API
//! - These are the transport clients (?)
//! ### High-Level API
//! - These are the domain clients (?)
//! - What features are available here? e.g. polling strategies, partitioning strategies
//! ### Streaming API
//!
//! - Low-level API implements the transport to directly access these commands.
//! - High-level API to instantiate consumers and producers.
//! [^note]: The details of how that is done internally are not discussed here. Refer, e.g. to [`the blog`].
//!
//! [`Apache Incubating Program`]: https://incubator.apache.org/
//! [`Apache Software Foundation`]: https://www.apache.org/
//! [`the blog`]: https://iggy.apache.org/blogs/2025/11/17/websocket-io-uring/
//! [`core/foreign`]: https://github.com/apache/iggy/tree/master/foreign
//! [`Designing Data-Intensive Applications`]: https://www.oreilly.com/library/view/designing-data-intensive-applications/9781491903063/
//! [`schema spec`]: https://iggy.apache.org/docs/server/schema/
//! [`server command enum`]: https://github.com/apache/iggy/blob/3e27ebc8dd5dbf257b816993908dc0747c4f8849/core/server/src/binary/command.rs#L74
//! [`website`]: https://iggy.apache.org/docs/introduction/architecture/

pub mod binary;
pub mod client_provider;
pub mod client_wrappers;
pub mod clients;
pub mod consumer_ext;
pub mod http;
mod leader_aware;
pub mod prelude;
pub mod quic;
pub mod session;
pub mod stream_builder;
pub mod tcp;
pub mod websocket;
