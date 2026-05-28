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

// * What, Why, How
// * Extensive use of links. Maybe use a linkchecker (github action: https://github.com/lycheeverse/lychee-action/)

//! Apache Iggy is a high-performance, persistent message streaming platform written in Rust, capable of processing millions of messages per second with ultra-low latency.
//! It is part of the [`Incubating Program`] of the [`Apache Software Foundation (ASF)`].
//!
//! To build with Iggy you will need a server instance and a SDK.
//! The source code for both is available for [download on our website](https://iggy.apache.org/downloads/) or on [GitHub](https://github.com/apache/iggy/).
//! This is the documentation for the Rust SDK. Other (foreign) language SDKs are [available](https://github.com/apache/iggy/tree/master/foreign).
//!
//! # Getting Started
//! We have three API tiers to build message-streaming applications, which provide different level of control dependent on the use case and needs.
//! For more tested examples check [GitHub](https://github.com/apache/iggy/tree/master/examples).
//!
//! ## High-level API
//! * name IggyConsumer features
//! * name IggyProducer features
//! This is most likely the interface you are looking for when you are starting out with Iggy.
//! Its intend is to abstract away transport specific implementation. Instead an IggyClient is provided that is transport agnostic.
//! The client connects to the server and allows to spawn producers and consumers.
//!
//! ### Example
//! ```rust
//!
//! ```
//! For more details on the high-level API refer to [`clients`]
//!
//!
//!
//!
//! # Background
//! ## What is message streaming?
//! ## Why choose Apache Iggy?
//! Message-streaming solutions are relevant in contexts where a lot of data is exchanged between many applications running on different devices.
//! When sending and retrieving data between devices one wants to ensure, that the source device sends the data and the destination device receives the data.
//! In distributed environments, where many systems, services and devices need to exchange data this can become error prone. Connections can fail, data can be corrupted or devices might fail entirely.
//!
//! Message-streaming solutions like Apache Iggy are used when a lot of data needs to be exchanged between services and devices. Usually, these data objects are called messages or events.
//! A system that depends on many interconnected applications ensuring data consistent data exchange is one of the key concerns. Two major challenges in these environments are *reliability* and *performance*.
//! Consider the following example: When you drive your vehicle around town many sensors are active and send telemetry data back to the manufacturer. This data is consumed and analyzed by many teams such as aftersales, product teams, production, development, data science and many more.
//! Imagine hundred thousands of cars each sending the sensor data to each team and their analyis applications individually.
//! This would likely allow a lot of points of failure. Sending sensors and receiving applications can be temporarily be unavailable or fail entirely, replicating the same data multiple times
//! to send it via peer-to-peer connections has serious performance implications.[^note]
//! To circumvent reliability and performance issues, sensors that produce events do not send data directly to the consuming applications. Instead they go through a data buffer. A server where the data is temporarily stored in a very simple data structure.
//! This is conceptionally not very different from typical data bases like Postgres and others.[^note] But, both serve different use cases. Data bases allow CRUD operations on complex data models and efficient querying. Message-streaming in contrast
//! is I/O optimized and build to temporarily park raw data that can easily be polled. Hence, on a technical level, the interfaces and gurantees vary substantially and are optimized for the intended use cases.
//! Visit our website for a [deep dive into the append-only data structure]() or check our blog post and the story of [how we guarantee ultra high performance on the server side]().
//!
//! The core of Iggy is a persisted append-only log data structure.
//! The Iggy server is build around this data structure and consists of logic to allow consumers and producers to read and write data to and from that log.
//! For more details visit our [`website`](). Deep dives into design decisions and server internals to ensure high-performance standards are outlined in various extensive [`blog posts`]().
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
//! # Mental Model
//! Apache Iggy Messages consist of 64 byte headers representing metadata and the actual message payload.
//! Each message that arrives at the server is contiguously appended to a binary *.log* file, the **segment**.
//! However, you might not want to send all messages to the same *.log* file since you are operating in an environemnt with domain boundaries,
//! here messages belong to certain applications or teams.
//! With **streams** and **topics** you can build two-level hirarchies to isolate messages into well defined boundaries.
//! A stream is essentially a namespace and topics an entity that defines how a category of messages is stored (e.g. configuring compression and retention periods).
//! All messages that belong to a specific topic are distributed across **partitions**.
//! Partitions are not another logical hierarchy level. They exist to enable parallel processing in cases where many consumers need to read from the same topic.
//! Within partitions you will find the *.log* file segement, such that on disk the outline will look something like this:
//! ```text
//! local_data/
//! ├── info.json
//! ├── state.messages
//! └── streams/
//!    └── 1/
//!        └── topics/
//!            └── 1/
//!                └── partitions/
//!                    └── 1/
//!                        ├── 00000000000000000000.index
//!                        └── 00000000000000000000.log
//! ```
//! We only touch briefly here on these concepts for completeness. You can find more details in the [architecture section](https://iggy.apache.org/docs/introduction/architecture/) on our website.
//!
//! Obviously, the server has quite elaborate implementations on how the messages end up in a .log file.
//! The good thing is, that we do not need to take care of that. We just need to make sure to ship the message (payload) to the server's doorstep with some metadata (headers) so that that the server can route
//! it to the correct .log file. The rest is abstracted away for us.
//!
//! To manipulate and interact with the server it exposes an interface called **commands**.
//! **Commands** are a set of defined operations that the server understands and alters either its own state or the managed messages.
//! The SDK can be understood as a wrapper around server commands and nothing else.
//! Wrapping means, that we take the server commands and add additional functionality and logic that is frequently required when building message-streaming solutions.
//! Basically, reuseable building blocks that would most likely be implemented by every application team otherwise.
//!
//! A complete server schema with all available commands can be found [here](https://iggy.apache.org/docs/server/schema/).
//!
//! # The three API tiers
//!
//! # The different API tiers
//! * Stream builder with [`IggyStream`].
//! * High-level API with [`IggyClient`], [`IggyProducer`] and [`IggyConsumer`].
//! * Low-level API with transport level clients [`TcpClient`], [`HttpClient`], [`QuicClient`] and [`WebsocketClient`].
//! The low level API wrapps the server commands around the specific transport protocol which allows deep control and building custom abstractions.
//! It does not implement things like auto-batching of messages, consumer group lifecycle, offset commits, retry logic or reconnection.
//! This is what the high-level API is for. It wrapps the low-level API and comes with frequently required features in message-streaming applications.
//! The high-level API is most likely the one you are looking for.
//! On the top level we provide the stream builder API, which builds the IggyClient, IggyProducer and IggyConsumer from the high-level API with one single configuration.
//!
//! ## Where to start
//! - **Evaluating Iggy or building a quick prototype?** → [`IggyStream`] (Convenience API)
//! - **Building a production application?** → [`IggyClient`] + [`IggyProducer`] / [`IggyConsumer`] (Domain API)
//! - **Writing a custom connector or integration?** → [`TcpClient`] or [`QuicClient`] (Transport API)
//! - **Working examples:** see the [`examples`] directory in the repository.
//! - **Questions?** Join our [`discord`] — maintainers and contributors are happy to help.
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
//! [^note]: The details of how that is done internally are not discussed here. Refer, e.g. to [`the blog`].
//! [^note]: tba
//! [^note]: Indeed Apache Iggy comes with many implemented [*connectors* or *sinks*]() where data buffered on iggy can be pushed to a database for long term storage.
//!
//! [`Incubating Program`]: https://incubator.apache.org/
//! [`Apache Software Foundation (ASF)`]: https://www.apache.org/foundation
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
