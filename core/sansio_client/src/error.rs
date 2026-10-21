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

//! The errors of the machine. `IggyError` lives in `iggy_common`, which
//! needs `std`, so this crate keeps its own type. A server code that the
//! machine does not act on stays a number.

use core::fmt;
use iggy_binary_protocol::WireError;
use iggy_binary_protocol::consensus::EvictionReason;

/// The `IggyError` codes that the machine reads or reports.
pub mod codes {
    pub const INVALID_COMMAND: u32 = 3;
    pub const INVALID_FORMAT: u32 = 4;
    pub const FEATURE_UNAVAILABLE: u32 = 5;
    pub const DISCONNECTED: u32 = 8;
    pub const STALE_CLIENT: u32 = 30;
    pub const UNAUTHENTICATED: u32 = 40;
    pub const INVALID_CREDENTIALS: u32 = 42;
    pub const INVALID_PERSONAL_ACCESS_TOKEN: u32 = 53;
    pub const TRANSIENT_NOT_COMMITTED: u32 = 57;
    pub const TRANSIENT_NOT_ACCEPTED: u32 = 58;
    pub const NOT_CONNECTED: u32 = 61;
    pub const CLIENT_SHUTDOWN: u32 = 63;
    pub const HISTORY_UNAVAILABLE: u32 = 87;
    pub const LIFECYCLE_BUSY: u32 = 88;
    pub const NOT_PRIMARY: u32 = 89;
    pub const PARTITION_NOT_FOUND: u32 = 3007;
    pub const CONSUMER_GROUP_PARTITION_NOT_OWNED: u32 = 5009;
    pub const INVALID_SESSION: u32 = 14001;
    pub const SESSION_MISMATCH: u32 = 14004;
    pub const REQUEST_ID_EXHAUSTED: u32 = 14005;
    pub const OPERATION_UNCERTAIN: u32 = 14006;
    pub const LANE_BLOCKED: u32 = 14007;
    pub const OPERATION_NOT_RETAINED: u32 = 14008;
}

/// Why a request failed, or why the machine refused an input.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientError {
    /// A server refusal that the machine does not act on, as its
    /// `IggyError` code.
    Server(u32),
    /// The server no longer attaches this connection to the session. The
    /// session lives on, and the next bind resumes it.
    StaleClient,
    /// The request needs a session and there is none.
    Unauthenticated,
    /// The server did not commit the request. The outcome is unknown.
    TransientNotCommitted,
    /// The server did not admit the request. It had no effect.
    TransientNotAccepted,
    /// The partition history that the request named is gone.
    HistoryUnavailable,
    /// The server is busy with a session lifecycle step.
    LifecycleBusy,
    /// The node is not the primary for the request.
    NotPrimary,
    /// The partition does not exist.
    PartitionNotFound,
    /// The consumer group member does not own the partition.
    PartitionNotOwned,
    /// The server ended the session.
    Evicted {
        reason: EvictionReason,
        server_protocol_version: u32,
        server_protocol_version_min: u32,
    },
    /// No reply arrived before the deadline.
    Timeout,
    /// The connection went away before the reply.
    Disconnected,
    /// The reply did not match its request, or did not decode.
    InvalidReply,
    /// The request could not be encoded.
    Wire(WireError),
    /// The command uses a feature that the machine does not support yet.
    Unsupported,
    /// The request counter reached its end.
    RequestIdExhausted,
    /// The wait queue is full.
    QueueFull,
    /// Another request already uses this handle.
    DuplicateHandle,
    /// The connection is closed.
    Closed,
    /// A login needs a logout first, because a session is bound.
    AlreadySignedIn,
    /// The server answered a bind with another session.
    SessionMismatch { expected: u64, actual: u64 },
    /// The server answered a login or a bind with session 0.
    InvalidSession,
    /// The write can have taken effect, and its lane retains it. Send
    /// [`Command::Resolve`](crate::Command::Resolve) to replay it, or call
    /// [`Connection::abandon`](crate::Connection::abandon).
    OperationUncertain(u64),
    /// The lane of the request retains the uncertain operation `op`.
    LaneBlocked(u64),
    /// No lane retains the operation, and no outcome of it is known.
    OperationNotRetained(u64),
}

impl ClientError {
    /// The error for a server code.
    #[must_use]
    pub const fn from_server_code(code: u32) -> Self {
        match code {
            codes::STALE_CLIENT => Self::StaleClient,
            codes::UNAUTHENTICATED => Self::Unauthenticated,
            codes::TRANSIENT_NOT_COMMITTED => Self::TransientNotCommitted,
            codes::TRANSIENT_NOT_ACCEPTED => Self::TransientNotAccepted,
            codes::HISTORY_UNAVAILABLE => Self::HistoryUnavailable,
            codes::LIFECYCLE_BUSY => Self::LifecycleBusy,
            codes::NOT_PRIMARY => Self::NotPrimary,
            codes::PARTITION_NOT_FOUND => Self::PartitionNotFound,
            codes::CONSUMER_GROUP_PARTITION_NOT_OWNED => Self::PartitionNotOwned,
            code => Self::Server(code),
        }
    }

    /// The `IggyError` code of the error, if one exists. `Timeout` and
    /// `Wire` have none, because the SDK reports them in other ways.
    #[must_use]
    pub const fn code(&self) -> Option<u32> {
        Some(match self {
            Self::Server(code) => *code,
            Self::StaleClient => codes::STALE_CLIENT,
            Self::Unauthenticated => codes::UNAUTHENTICATED,
            Self::TransientNotCommitted => codes::TRANSIENT_NOT_COMMITTED,
            Self::TransientNotAccepted | Self::QueueFull => codes::TRANSIENT_NOT_ACCEPTED,
            Self::HistoryUnavailable => codes::HISTORY_UNAVAILABLE,
            Self::LifecycleBusy => codes::LIFECYCLE_BUSY,
            Self::NotPrimary => codes::NOT_PRIMARY,
            Self::PartitionNotFound => codes::PARTITION_NOT_FOUND,
            Self::PartitionNotOwned => codes::CONSUMER_GROUP_PARTITION_NOT_OWNED,
            Self::Evicted { reason, .. } => match reason {
                EvictionReason::InvalidCredentials => codes::INVALID_CREDENTIALS,
                EvictionReason::InvalidToken => codes::INVALID_PERSONAL_ACCESS_TOKEN,
                EvictionReason::StaleClient => codes::STALE_CLIENT,
                EvictionReason::MalformedLogin => codes::INVALID_FORMAT,
                _ => codes::UNAUTHENTICATED,
            },
            Self::Disconnected => codes::DISCONNECTED,
            Self::InvalidReply => codes::INVALID_COMMAND,
            Self::Unsupported => codes::FEATURE_UNAVAILABLE,
            Self::RequestIdExhausted => codes::REQUEST_ID_EXHAUSTED,
            Self::Closed => codes::CLIENT_SHUTDOWN,
            Self::SessionMismatch { .. } => codes::SESSION_MISMATCH,
            Self::InvalidSession => codes::INVALID_SESSION,
            Self::OperationUncertain(_) => codes::OPERATION_UNCERTAIN,
            Self::LaneBlocked(_) => codes::LANE_BLOCKED,
            Self::OperationNotRetained(_) => codes::OPERATION_NOT_RETAINED,
            Self::Timeout | Self::Wire(_) | Self::DuplicateHandle | Self::AlreadySignedIn => {
                return None;
            }
        })
    }

    /// Whether the request failed because the connection went away.
    pub(crate) const fn is_lost_link(&self) -> bool {
        matches!(self, Self::Disconnected | Self::Timeout)
    }
}

impl From<WireError> for ClientError {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server(code) => write!(f, "the server refused the request with code {code}"),
            Self::StaleClient => f.write_str("the server detached the connection"),
            Self::Unauthenticated => f.write_str("the request needs a session"),
            Self::TransientNotCommitted => f.write_str("the server did not commit the request"),
            Self::TransientNotAccepted => f.write_str("the server did not admit the request"),
            Self::HistoryUnavailable => f.write_str("the partition history is gone"),
            Self::LifecycleBusy => f.write_str("the server is busy with a session lifecycle step"),
            Self::NotPrimary => f.write_str("the node is not the primary"),
            Self::PartitionNotFound => f.write_str("the partition does not exist"),
            Self::PartitionNotOwned => f.write_str("the consumer does not own the partition"),
            Self::Evicted { reason, .. } => write!(f, "the server ended the session: {reason:?}"),
            Self::Timeout => f.write_str("no reply before the deadline"),
            Self::Disconnected => f.write_str("the connection went away"),
            Self::InvalidReply => f.write_str("the reply is invalid"),
            Self::Wire(error) => write!(f, "the request does not encode: {error}"),
            Self::Unsupported => f.write_str("the command is not supported"),
            Self::RequestIdExhausted => f.write_str("the request counter reached its end"),
            Self::QueueFull => f.write_str("the wait queue is full"),
            Self::DuplicateHandle => f.write_str("another request uses this handle"),
            Self::Closed => f.write_str("the connection is closed"),
            Self::AlreadySignedIn => f.write_str("a session is bound, log out first"),
            Self::SessionMismatch { expected, actual } => {
                write!(f, "the server bound session {actual}, not {expected}")
            }
            Self::InvalidSession => f.write_str("the server answered with session 0"),
            Self::OperationUncertain(op) => write!(f, "the outcome of operation {op} is unknown"),
            Self::LaneBlocked(op) => write!(f, "the lane retains operation {op}"),
            Self::OperationNotRetained(op) => write!(f, "operation {op} is not retained"),
        }
    }
}

impl core::error::Error for ClientError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_server_codes_round_trip() {
        for code in [30, 40, 57, 58, 87, 88, 89, 3007, 5009, 1234] {
            assert_eq!(ClientError::from_server_code(code).code(), Some(code));
        }
        assert_eq!(
            ClientError::from_server_code(57),
            ClientError::TransientNotCommitted
        );
        assert_eq!(
            ClientError::from_server_code(1234),
            ClientError::Server(1234)
        );
    }
}
