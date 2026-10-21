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

//! The commands that an application sends, and their replies.

use crate::error::ClientError;
use crate::route::PartitionKey;
use alloc::string::String;
use alloc::vec::Vec;
use bytes::{Bytes, BytesMut};
use iggy_binary_protocol::codes::{
    DELETE_CONSUMER_OFFSET_CODE, GET_CONSUMER_OFFSET_CODE, LOGIN_REGISTER_CODE,
    LOGIN_REGISTER_WITH_PAT_CODE, LOGOUT_USER_CODE, PING_CODE, POLL_MESSAGES_CODE,
    SEND_MESSAGES_CODE, STORE_CONSUMER_OFFSET_CODE,
};
use iggy_binary_protocol::requests::consumer_offsets::{
    DeleteConsumerOffsetRequest, GetConsumerOffsetRequest, StoreConsumerOffsetRequest,
};
use iggy_binary_protocol::requests::messages::{
    PollMessagesRequest, RawMessage, SendMessagesEncoder, SendMessagesHeader,
};
use iggy_binary_protocol::requests::system::PingRequest;
use iggy_binary_protocol::requests::users::login_register::BindSecret;
use iggy_binary_protocol::requests::users::{
    LoginRegisterRequest, LoginRegisterWithPatRequest, LogoutUserRequest,
};
use iggy_binary_protocol::responses::messages::{
    PollMessagesResponse, PollMessagesResponseHeader, poll_messages::PolledBatchesIterator,
};
use iggy_binary_protocol::responses::users::LoginRegisterResponse;
use iggy_binary_protocol::{
    ClientVersionInfo, KIND_CONSUMER, WireDecode, WireEncode, WireError, WireIdentifier, WireName,
    WirePartitioning, WirePollingStrategy,
};
use secrecy::{ExposeSecret, SecretString};

/// The longest password or token that the wire can carry: one length byte.
const MAX_SECRET_LENGTH: usize = 255;

/// One request of the application.
#[derive(Debug, Clone)]
pub enum Command {
    /// Signs in with a user name and a password, and binds a new session.
    LoginUser {
        username: String,
        password: SecretString,
    },
    /// Signs in with a personal access token, and binds a new session.
    LoginWithToken { token: SecretString },
    /// Asks the server for a sign of life.
    Ping,
    /// Ends the session. The machine then makes a new identity.
    Logout,
    /// Appends messages to one partition.
    SendMessages {
        stream_id: WireIdentifier,
        topic_id: WireIdentifier,
        partition_id: u32,
        messages: Vec<Message>,
    },
    /// Reads messages from one partition.
    PollMessages(PollMessagesRequest),
    /// Stores the offset of a consumer in one partition.
    StoreConsumerOffset(StoreConsumerOffsetRequest),
    /// Replays the retained operation `op` until its outcome is known.
    Resolve(u64),
    /// Any other command, as its code and body. The reply is the raw body.
    /// A partition command gets a route, as a typed one does.
    Raw { code: u32, payload: Bytes },
}

/// One message to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The id of the message. Zero lets the server pick one.
    pub id: u128,
    /// The time the message was made, in microseconds since the Unix epoch.
    pub origin_timestamp: u64,
    /// User headers in the wire format of `encode_user_headers`.
    pub headers: Option<Bytes>,
    pub payload: Bytes,
}

impl Message {
    pub const fn new(payload: Bytes) -> Self {
        Self {
            id: 0,
            origin_timestamp: 0,
            headers: None,
            payload,
        }
    }
}

/// The reply to one [`Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    SignedIn(LoginRegisterResponse),
    Pong,
    LoggedOut,
    Sent,
    Polled(PolledMessages),
    OffsetStored,
    /// The replayed operation committed.
    Resolved,
    Raw(Bytes),
}

/// The reply to a poll. It keeps the reply body, so the messages are not
/// copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolledMessages {
    pub header: PollMessagesResponseHeader,
    body: Bytes,
}

impl PolledMessages {
    fn decode(body: Bytes) -> Result<Self, ClientError> {
        let header = PollMessagesResponse::decode(&body)
            .map_err(|_| ClientError::InvalidReply)?
            .header;
        Ok(Self { header, body })
    }

    /// The polled messages, read in place from the reply body.
    pub fn messages(&self) -> PolledBatchesIterator<'_> {
        // The body decoded once already, so it decodes again.
        PollMessagesResponse::decode(&self.body).map_or_else(
            |_| PolledBatchesIterator::new(&[]),
            |response| response.messages,
        )
    }

    /// The whole reply body.
    pub const fn body(&self) -> &Bytes {
        &self.body
    }
}

/// What a call does, which decides how its reply decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    SignIn,
    Ping,
    Logout,
    Send,
    Poll,
    StoreOffset,
    Resolve,
    Raw,
}

/// The body of a call. A sign-in body carries the bind secret, so it is
/// built when the frame freezes, with the secret of the current identity.
#[derive(Debug, Clone)]
pub enum Payload {
    Bytes(Bytes),
    SignIn(SignIn),
}

#[derive(Debug, Clone)]
pub enum SignIn {
    Password {
        username: WireName,
        password: SecretString,
    },
    Token(SecretString),
}

impl SignIn {
    pub const fn code(&self) -> u32 {
        match self {
            Self::Password { .. } => LOGIN_REGISTER_CODE,
            Self::Token(_) => LOGIN_REGISTER_WITH_PAT_CODE,
        }
    }

    pub fn encode(&self, version_info: ClientVersionInfo, bind_secret: BindSecret) -> Bytes {
        match self {
            Self::Password { username, password } => LoginRegisterRequest {
                version_info,
                bind_secret,
                username: username.clone(),
                password: password.clone(),
                client_context: None,
            }
            .to_bytes(),
            Self::Token(token) => LoginRegisterWithPatRequest {
                version_info,
                bind_secret,
                token: token.clone(),
                client_context: None,
            }
            .to_bytes(),
        }
    }
}

/// The partition of a partition call.
#[derive(Debug, Clone)]
pub struct Partition {
    pub key: PartitionKey,
    /// A poll that advances the stored offset, so a replay can advance it
    /// again. It takes its partition lane while active.
    pub progress_poll: bool,
}

/// A command, ready for the machine.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub kind: Kind,
    pub code: u32,
    pub payload: Payload,
    pub partition: Option<Partition>,
}

impl Command {
    /// Encodes the command. `Resolve` has no body of its own and returns
    /// `None`.
    ///
    /// # Errors
    ///
    /// Returns `Wire` for a field that does not encode, and `Unsupported`
    /// for a partition that is not named by id, or a raw login.
    pub fn encode(self) -> Result<Option<Encoded>, ClientError> {
        let (kind, code, payload) = match self {
            Self::LoginUser { username, password } => {
                let username = WireName::new(username)?;
                check_secret(password.expose_secret())?;
                let sign_in = SignIn::Password { username, password };
                (Kind::SignIn, sign_in.code(), Payload::SignIn(sign_in))
            }
            Self::LoginWithToken { token } => {
                check_secret(token.expose_secret())?;
                let sign_in = SignIn::Token(token);
                (Kind::SignIn, sign_in.code(), Payload::SignIn(sign_in))
            }
            Self::Ping => (
                Kind::Ping,
                PING_CODE,
                Payload::Bytes(PingRequest.to_bytes()),
            ),
            Self::Logout => (
                Kind::Logout,
                LOGOUT_USER_CODE,
                Payload::Bytes(LogoutUserRequest.to_bytes()),
            ),
            Self::SendMessages {
                stream_id,
                topic_id,
                partition_id,
                messages,
            } => {
                let partitioning = WirePartitioning::PartitionId(partition_id);
                let raw: Vec<RawMessage<'_>> = messages
                    .iter()
                    .map(|message| RawMessage {
                        id: message.id,
                        origin_timestamp: message.origin_timestamp,
                        headers: message.headers.as_deref(),
                        payload: &message.payload,
                    })
                    .collect();
                let mut body = BytesMut::with_capacity(SendMessagesEncoder::encoded_size(
                    &stream_id,
                    &topic_id,
                    &partitioning,
                    &raw,
                ));
                SendMessagesEncoder::encode(&mut body, &stream_id, &topic_id, &partitioning, &raw)?;
                (
                    Kind::Send,
                    SEND_MESSAGES_CODE,
                    Payload::Bytes(body.freeze()),
                )
            }
            Self::PollMessages(request) => (
                Kind::Poll,
                POLL_MESSAGES_CODE,
                Payload::Bytes(request.to_bytes()),
            ),
            Self::StoreConsumerOffset(request) => (
                Kind::StoreOffset,
                STORE_CONSUMER_OFFSET_CODE,
                Payload::Bytes(request.to_bytes()),
            ),
            Self::Resolve(_) => return Ok(None),
            Self::Raw { code, payload } => match code {
                LOGIN_REGISTER_CODE | LOGIN_REGISTER_WITH_PAT_CODE => {
                    // The body carries the bind secret, which only the machine knows.
                    return Err(ClientError::Unsupported);
                }
                _ => (Kind::Raw, code, Payload::Bytes(payload)),
            },
        };
        let partition = match &payload {
            Payload::Bytes(body) => partition_of(code, body)?,
            Payload::SignIn(_) => None,
        };
        Ok(Some(Encoded {
            kind,
            code,
            payload,
            partition,
        }))
    }
}

/// The partition that a request of `code` addresses, as `send_raw` in
/// `core/sdk/src/driver.rs` reads it. `None` for a request without one.
fn partition_of(code: u32, body: &[u8]) -> Result<Option<Partition>, ClientError> {
    let (stream_id, topic_id, partition_id, progress_poll) = match code {
        SEND_MESSAGES_CODE => {
            let (metadata_length, rest) = body
                .split_first_chunk::<4>()
                .ok_or(ClientError::Wire(eof()))?;
            let header = rest
                .get(..u32::from_le_bytes(*metadata_length) as usize)
                .ok_or(ClientError::Wire(eof()))
                .and_then(|metadata| Ok(SendMessagesHeader::decode_from(metadata)?))?;
            let WirePartitioning::PartitionId(partition_id) = header.partitioning else {
                return Err(ClientError::Unsupported);
            };
            (header.stream_id, header.topic_id, partition_id, false)
        }
        POLL_MESSAGES_CODE => {
            let request = PollMessagesRequest::decode_from(body)?;
            let partition_id = match request.partition_id {
                Some(partition_id) => partition_id,
                None if request.consumer.kind == KIND_CONSUMER => 0,
                None => return Err(ClientError::Unsupported),
            };
            let progress =
                request.auto_commit || request.strategy.kind == WirePollingStrategy::next().kind;
            (request.stream_id, request.topic_id, partition_id, progress)
        }
        GET_CONSUMER_OFFSET_CODE => {
            let request = GetConsumerOffsetRequest::decode_from(body)?;
            let partition_id = request.partition_id.unwrap_or(0);
            (request.stream_id, request.topic_id, partition_id, false)
        }
        STORE_CONSUMER_OFFSET_CODE => {
            let request = StoreConsumerOffsetRequest::decode_from(body)?;
            let partition_id = request.partition_id.ok_or(ClientError::Unsupported)?;
            (request.stream_id, request.topic_id, partition_id, false)
        }
        DELETE_CONSUMER_OFFSET_CODE => {
            let request = DeleteConsumerOffsetRequest::decode_from(body)?;
            let partition_id = request.partition_id.ok_or(ClientError::Unsupported)?;
            (request.stream_id, request.topic_id, partition_id, false)
        }
        _ => return Ok(None),
    };
    Ok(Some(Partition {
        key: PartitionKey::new(stream_id, topic_id, partition_id),
        progress_poll,
    }))
}

/// Decodes the reply body of a call of `kind`.
pub fn decode_reply(kind: Kind, body: Bytes) -> Result<Reply, ClientError> {
    Ok(match kind {
        Kind::SignIn => Reply::SignedIn(
            LoginRegisterResponse::decode_from(&body).map_err(|_| ClientError::InvalidReply)?,
        ),
        Kind::Ping => Reply::Pong,
        Kind::Logout => Reply::LoggedOut,
        Kind::Send => Reply::Sent,
        Kind::Poll => Reply::Polled(PolledMessages::decode(body)?),
        Kind::StoreOffset => Reply::OffsetStored,
        Kind::Resolve => Reply::Resolved,
        Kind::Raw => Reply::Raw(body),
    })
}

const fn check_secret(secret: &str) -> Result<(), ClientError> {
    if secret.is_empty() || secret.len() > MAX_SECRET_LENGTH {
        return Err(ClientError::Wire(WireError::Validation(
            alloc::borrow::Cow::Borrowed("a password or token must have 1 to 255 bytes"),
        )));
    }
    Ok(())
}

const fn eof() -> WireError {
    WireError::UnexpectedEof {
        offset: 0,
        need: 4,
        have: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iggy_binary_protocol::WireConsumer;

    fn poll(strategy: WirePollingStrategy, auto_commit: bool) -> Command {
        Command::PollMessages(PollMessagesRequest {
            consumer: WireConsumer::consumer(WireIdentifier::numeric(1)),
            stream_id: WireIdentifier::numeric(1),
            topic_id: WireIdentifier::numeric(2),
            partition_id: Some(3),
            strategy,
            count: 10,
            auto_commit,
        })
    }

    #[test]
    fn a_send_addresses_its_partition() {
        let encoded = Command::SendMessages {
            stream_id: WireIdentifier::numeric(1),
            topic_id: WireIdentifier::numeric(2),
            partition_id: 3,
            messages: alloc::vec![Message::new(Bytes::from_static(b"hello"))],
        }
        .encode()
        .unwrap()
        .unwrap();
        let partition = encoded.partition.unwrap();
        assert_eq!(partition.key.partition_id(), 3);
        assert!(!partition.progress_poll);
    }

    #[test]
    fn a_next_or_auto_commit_poll_is_a_progress_poll() {
        for (strategy, auto_commit, progress) in [
            (WirePollingStrategy::offset(0), false, false),
            (WirePollingStrategy::offset(0), true, true),
            (WirePollingStrategy::next(), false, true),
        ] {
            let encoded = poll(strategy, auto_commit).encode().unwrap().unwrap();
            assert_eq!(encoded.partition.unwrap().progress_poll, progress);
        }
    }

    #[test]
    fn an_empty_password_does_not_encode() {
        let login = Command::LoginUser {
            username: "iggy".into(),
            password: SecretString::from(""),
        };
        assert!(matches!(login.encode(), Err(ClientError::Wire(_))));
    }
}
