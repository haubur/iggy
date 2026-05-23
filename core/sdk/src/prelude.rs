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

//! Prelude module for the Iggy SDK.
//!
//! This module re-exports the most common types, traits, and functions
//! from the Iggy SDK to make them easier to import and use.
//!
//! The only thing you need to do is to import like this:
//! ```
//! use iggy::prelude::*;
//! ```
//! After that you do not need to be familiar with the crate and module layout.
//! Instead of importing the consumer and producer builder like this
//! ```
//! use iggy::clients::consumer_builder::IggyConsumerBuilder;
//! use iggy::clients::producer_builder::IggyProducerBuilder;
//! ```
//! you can simply import the prelude once and instantiate them
//! ```
//! use iggy::prelude::*;
//!
//! let producer = IggyConsumerBuilder::new();
//! let consumer = IggyConsumerBuilder::new();
//! ```
//!
//! The prelude re-exports a lot of things for convinience.
//! If you are just starting out and need guidance on what the SDK's APIs provide, check the lib.rs where you will also find some references to deep dives on our website.
//!
//! You will find the high-level API in [`clients`] module,
//! the low-level API in the modules [`http`], [`quic`], [`tcp`] and [`websocket`].
//! A quick start abstraction wrapping the high-level API for the easiest way to get started in the [`stream_builder`] module.

pub use crate::client_provider;
pub use crate::client_provider::ClientProviderConfig;
pub use crate::client_wrappers::client_wrapper::ClientWrapper;
pub use crate::client_wrappers::connection_info::ConnectionInfo;
pub use crate::clients::client::IggyClient;
pub use crate::clients::client_builder::IggyClientBuilder;
pub use crate::clients::consumer::{
    AutoCommit, AutoCommitAfter, AutoCommitWhen, IggyConsumer, ReceivedMessage,
};
pub use crate::clients::consumer_builder::IggyConsumerBuilder;
pub use crate::clients::producer::IggyProducer;
pub use crate::clients::producer_builder::IggyProducerBuilder;
pub use crate::clients::producer_config::{BackgroundConfig, DirectConfig};
pub use crate::clients::producer_sharding::{BalancedSharding, OrderedSharding, Sharding};
pub use crate::consumer_ext::IggyConsumerMessageExt;
pub use crate::stream_builder::IggyConsumerConfig;
pub use crate::stream_builder::IggyStreamConsumer;
pub use crate::stream_builder::{IggyProducerConfig, IggyStreamProducer};
pub use crate::stream_builder::{IggyStream, IggyStreamConfig};
pub use crate::tcp::tcp_client::TcpClient;
pub use crate::websocket::websocket_client::WebSocketClient;
pub use iggy_common::{
    Aes256GcmEncryptor, Args, ArgsOptional, AutoLogin, CacheMetrics, CacheMetricsKey, ClientError,
    ClientInfoDetails, ClusterMetadata, ClusterNode, ClusterNodeRole, ClusterNodeStatus,
    CompressionAlgorithm, Consumer, ConsumerGroupDetails, ConsumerKind, EncryptorKind,
    GlobalPermissions, HeaderKey, HeaderKind, HeaderValue, HttpClientConfig,
    HttpClientConfigBuilder, IdKind, Identifier, IdentityInfo, IggyByteSize, IggyDuration,
    IggyError, IggyExpiry, IggyIndexView, IggyMessage, IggyMessageHeader, IggyMessageHeaderView,
    IggyMessageView, IggyMessageViewIterator, IggyTimestamp, MaxTopicSize, Partition, Partitioner,
    Partitioning, Permissions, PersonalAccessTokenExpiry, PollMessages, PolledMessages,
    PollingKind, PollingStrategy, QuicClientConfig, QuicClientConfigBuilder,
    QuicClientReconnectionConfig, SendMessages, Sizeable, SnapshotCompression, Stats, Stream,
    StreamDetails, StreamPermissions, SystemSnapshotType, TcpClientConfig, TcpClientConfigBuilder,
    TcpClientReconnectionConfig, Topic, TopicDetails, TopicPermissions, TransportEndpoints,
    TransportProtocol, UserId, UserStatus, Validatable, WebSocketClientConfig,
    WebSocketClientConfigBuilder, WebSocketClientReconnectionConfig, defaults, locking,
};
pub use iggy_common::{
    Client, ClusterClient, ConsumerGroupClient, ConsumerOffsetClient, MessageClient,
    PartitionClient, PersonalAccessTokenClient, SegmentClient, StreamClient, SystemClient,
    TopicClient, UserClient,
};
pub use iggy_common::{
    IGGY_MESSAGE_CHECKSUM_OFFSET_RANGE, IGGY_MESSAGE_HEADER_SIZE,
    IGGY_MESSAGE_HEADERS_LENGTH_OFFSET_RANGE, IGGY_MESSAGE_ID_OFFSET_RANGE,
    IGGY_MESSAGE_OFFSET_OFFSET_RANGE, IGGY_MESSAGE_ORIGIN_TIMESTAMP_OFFSET_RANGE,
    IGGY_MESSAGE_PAYLOAD_LENGTH_OFFSET_RANGE, IGGY_MESSAGE_TIMESTAMP_OFFSET_RANGE, INDEX_SIZE,
    MAX_PAYLOAD_SIZE, MAX_USER_HEADERS_SIZE, SEC_IN_MICRO,
    defaults::{DEFAULT_ROOT_PASSWORD, DEFAULT_ROOT_USER_ID, DEFAULT_ROOT_USERNAME},
};
