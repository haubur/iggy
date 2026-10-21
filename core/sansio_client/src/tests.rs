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

//! Scripted tests of the connection: bytes in, frames and completions out.

use crate::command::{Command, Message, Reply};
use crate::config::Config;
use crate::connection::{Completion, Connection, Input, Output, RequestHandle, Submit};
use crate::error::ClientError;
use crate::testing::{COMMIT, Counter, eviction, frame, refusal, reply};
use crate::time::Instant;
use alloc::vec;
use alloc::vec::Vec;
use bytes::Bytes;
use core::time::Duration;
use iggy_binary_protocol::codes::{
    BIND_SESSION_CODE, CREATE_STREAM_CODE, GET_PARTITION_ROUTE_CODE, LOGIN_REGISTER_CODE,
    LOGOUT_USER_CODE, PING_CODE, SEND_MESSAGES_CODE,
};
use iggy_binary_protocol::consensus::{
    Command as FrameCommand, EvictionReason, HEADER_SIZE, NON_REPLICATED_CODE_RANGE, Operation,
    ReplyHeader, RequestHeader,
};
use iggy_binary_protocol::responses::messages::PartitionRouteResponse;
use iggy_binary_protocol::responses::users::LoginRegisterResponse;
use iggy_binary_protocol::{WireEncode, WireIdentifier, WireName};
use sansio::Protocol;
use secrecy::SecretString;

const SESSION: u64 = 9;
const USER: u32 = 3;
const INCARNATION: u64 = 44;

/// One frame that the machine wrote.
struct Sent {
    header: RequestHeader,
    body: Bytes,
}

impl Sent {
    /// The command code: the code in `reserved` for a read, else the code
    /// of the operation.
    fn code(&self) -> u32 {
        match self.header.operation {
            Operation::NonReplicated => {
                let mut code = [0; 4];
                code.copy_from_slice(&self.header.reserved[NON_REPLICATED_CODE_RANGE]);
                u32::from_le_bytes(code)
            }
            Operation::Register => LOGIN_REGISTER_CODE,
            Operation::Logout => LOGOUT_USER_CODE,
            Operation::SendMessages => SEND_MESSAGES_CODE,
            Operation::CreateStream => CREATE_STREAM_CODE,
            operation => panic!("no code for {operation:?}"),
        }
    }

    const fn request(&self) -> u64 {
        self.header.request
    }
}

/// A connection, a clock, and the server side of the script.
struct Harness {
    conn: Connection<Counter>,
    now: Instant,
}

impl Harness {
    fn new() -> Self {
        let mut harness = Self {
            conn: Connection::new(Config::default(), Counter::default()).unwrap(),
            now: Instant::from_micros(1_000_000),
        };
        harness.tick(Duration::ZERO);
        harness
    }

    /// A connected harness with a bound session.
    fn signed_in() -> Self {
        let mut harness = Self::new();
        harness.connect();
        harness.submit(0, login());
        let register = harness.sent_one();
        assert_eq!(register.code(), LOGIN_REGISTER_CODE);
        harness.feed(&register_reply(register.request(), SESSION));
        assert!(matches!(
            harness.completions()[..],
            [Completion {
                result: Ok(Reply::SignedIn(_)),
                ..
            }]
        ));
        harness.events();
        harness
    }

    fn tick(&mut self, by: Duration) {
        self.now = self.now + by;
        self.conn.handle_timeout(self.now).unwrap();
    }

    fn connect(&mut self) {
        self.conn.handle_event(Input::Connected).unwrap();
    }

    fn disconnect(&mut self) {
        self.conn.handle_event(Input::Disconnected).unwrap();
    }

    fn submit(&mut self, handle: u64, command: Command) {
        self.submit_with(handle, command, None);
    }

    fn submit_with(&mut self, handle: u64, command: Command, timeout: Option<Duration>) {
        self.conn
            .handle_write(Submit {
                handle: RequestHandle(handle),
                command,
                timeout,
            })
            .unwrap();
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.conn
            .handle_read(Bytes::copy_from_slice(bytes))
            .unwrap();
    }

    fn sent(&mut self) -> Vec<Sent> {
        core::iter::from_fn(|| self.conn.poll_write())
            .map(|transmit| Sent {
                header: bytemuck::checked::try_pod_read_unaligned(&transmit.header).unwrap(),
                body: transmit.body,
            })
            .collect()
    }

    fn sent_one(&mut self) -> Sent {
        let mut sent = self.sent();
        assert_eq!(sent.len(), 1, "expected one frame");
        sent.remove(0)
    }

    fn completions(&mut self) -> Vec<Completion> {
        core::iter::from_fn(|| self.conn.poll_read()).collect()
    }

    fn events(&mut self) -> Vec<Output> {
        core::iter::from_fn(|| self.conn.poll_event()).collect()
    }
}

fn login() -> Command {
    Command::LoginUser {
        username: "iggy".into(),
        password: SecretString::from("iggy"),
    }
}

fn create_stream() -> Command {
    Command::Raw {
        code: CREATE_STREAM_CODE,
        payload: Bytes::from_static(b"stream"),
    }
}

fn send(payload: &'static [u8]) -> Command {
    Command::SendMessages {
        stream_id: WireIdentifier::numeric(1),
        topic_id: WireIdentifier::numeric(2),
        partition_id: 3,
        messages: vec![Message::new(Bytes::from_static(payload))],
    }
}

fn session_reply(request: u64, session: u64) -> Vec<u8> {
    let body = LoginRegisterResponse {
        user_id: USER,
        session,
        server_protocol_version: 1,
        server_version: WireName::new("test").unwrap(),
    }
    .to_bytes();
    reply(request, &body)
}

fn register_reply(request: u64, session: u64) -> Vec<u8> {
    let mut body = 0_u32.to_le_bytes().to_vec();
    body.extend_from_slice(
        &LoginRegisterResponse {
            user_id: USER,
            session,
            server_protocol_version: 1,
            server_version: WireName::new("test").unwrap(),
        }
        .to_bytes(),
    );
    frame(
        ReplyHeader {
            command: FrameCommand::Reply,
            operation: Operation::Register,
            request,
            ..Default::default()
        },
        &body,
    )
}

/// A reply that proves the commit of `request`, with an empty payload.
fn committed(request: &RequestHeader) -> Vec<u8> {
    frame(
        ReplyHeader {
            command: FrameCommand::Reply,
            operation: request.operation,
            client: request.client,
            request: request.request,
            request_checksum: request.request_checksum,
            partition_incarnation: request.partition_incarnation,
            op: COMMIT,
            commit: COMMIT,
            reserved: [ReplyHeader::COMMITTED_RESULT, 0, 0, 0],
            ..Default::default()
        },
        &0_u32.to_le_bytes(),
    )
}

fn route_reply(request: u64) -> Vec<u8> {
    let route = PartitionRouteResponse {
        incarnation: INCARNATION,
        metadata_op: COMMIT,
        view: 1,
        partitions_count: 4,
        primary_replica: 0,
    };
    reply(request, &route.to_bytes())
}

fn send_reply(request: u64) -> Vec<u8> {
    frame(
        ReplyHeader {
            command: FrameCommand::Reply,
            operation: Operation::SendMessages,
            request,
            ..Default::default()
        },
        &[],
    )
}

#[test]
fn a_login_registers_unbound_and_later_requests_use_the_session() {
    let mut harness = Harness::new();
    harness.connect();
    harness.submit(1, login());
    let register = harness.sent_one();
    assert_eq!(register.header.operation, Operation::Register);
    assert_eq!((register.header.session, register.request()), (0, 0));

    harness.feed(&register_reply(0, SESSION));
    assert_eq!(
        harness.events(),
        [Output::SignedIn {
            user_id: USER,
            session: SESSION,
            server_protocol_version: 1
        }]
    );
    assert_eq!(harness.completions()[0].handle, RequestHandle(1));

    harness.submit(2, Command::Ping);
    let ping = harness.sent_one();
    assert_eq!(ping.code(), PING_CODE);
    assert_eq!((ping.header.session, ping.request()), (SESSION, 1));
}

#[test]
fn a_call_that_needs_a_session_waits_for_the_login_on_its_way() {
    let mut harness = Harness::new();
    harness.connect();
    harness.submit(1, login());
    harness.submit(2, create_stream());
    assert_eq!(harness.sent().len(), 1);
    harness.feed(&register_reply(0, SESSION));
    let create = harness.sent_one();
    assert_eq!(create.code(), CREATE_STREAM_CODE);
    assert_eq!(create.header.session, SESSION);
}

#[test]
fn a_write_without_a_session_fails_at_once() {
    let mut harness = Harness::new();
    harness.connect();
    harness.submit(1, create_stream());
    assert_eq!(
        harness.completions()[0].result,
        Err(ClientError::Unauthenticated)
    );
}

#[test]
fn given_two_requests_in_flight_when_replies_arrive_reversed_should_deliver_each_to_its_caller() {
    let mut harness = Harness::signed_in();
    harness.submit(
        10,
        Command::Raw {
            code: PING_CODE,
            payload: Bytes::new(),
        },
    );
    harness.submit(
        11,
        Command::Raw {
            code: PING_CODE,
            payload: Bytes::new(),
        },
    );
    let sent = harness.sent();
    assert_eq!(sent.len(), 2);
    harness.feed(&reply(sent[1].request(), b"second"));
    harness.feed(&reply(sent[0].request(), b"first"));
    let completions = harness.completions();
    assert_eq!(completions[0].handle, RequestHandle(11));
    assert_eq!(
        completions[0].result,
        Ok(Reply::Raw(Bytes::from_static(b"second")))
    );
    assert_eq!(completions[1].handle, RequestHandle(10));
    assert_eq!(
        completions[1].result,
        Ok(Reply::Raw(Bytes::from_static(b"first")))
    );
}

#[test]
fn given_replies_split_into_single_bytes_should_deliver_each_reply() {
    let mut harness = Harness::signed_in();
    harness.submit(1, Command::Ping);
    harness.submit(2, Command::Ping);
    let sent = harness.sent();
    let mut stream = reply(sent[0].request(), b"");
    stream.extend(reply(sent[1].request(), b""));
    for byte in stream {
        harness.feed(&[byte]);
    }
    let handles: Vec<_> = harness.completions().iter().map(|c| c.handle).collect();
    assert_eq!(handles, [RequestHandle(1), RequestHandle(2)]);
}

#[test]
fn only_one_unbound_request_is_on_the_wire() {
    let mut harness = Harness::new();
    harness.connect();
    harness.submit(1, Command::Ping);
    harness.submit(2, Command::Ping);
    assert_eq!(harness.sent().len(), 1);
    harness.feed(&reply(0, b""));
    assert_eq!(harness.sent_one().request(), 0);
}

#[test]
fn given_a_cancelled_caller_when_its_late_reply_arrives_should_not_complete_the_next_request() {
    let mut harness = Harness::new();
    harness.connect();
    harness.submit_with(1, Command::Ping, Some(Duration::from_secs(1)));
    harness.sent_one();
    harness.tick(Duration::from_secs(2));
    assert_eq!(harness.completions()[0].result, Err(ClientError::Timeout));
    // A late reply with id 0 would answer the next unbound request.
    assert_eq!(harness.events(), [Output::CloseTransport]);

    harness.connect();
    harness.submit(2, Command::Ping);
    harness.sent_one();
    assert_eq!(harness.completions().len(), 0);
}

#[test]
fn given_an_eviction_when_requests_are_pending_should_fail_all_of_them_and_close_the_link() {
    let mut harness = Harness::signed_in();
    harness.submit(1, Command::Ping);
    harness.submit(2, create_stream());
    harness.sent();
    harness.feed(&eviction(EvictionReason::UserInactive));
    let completions = harness.completions();
    assert_eq!(completions.len(), 2);
    assert!(completions.iter().all(|completion| matches!(
        completion.result,
        Err(ClientError::Evicted {
            reason: EvictionReason::UserInactive,
            ..
        })
    )));
    let events = harness.events();
    assert!(matches!(events[0], Output::Evicted { .. }));
    assert_eq!(events[1], Output::CloseTransport);
    assert!(!harness.conn.is_signed_in());
}

#[test]
fn a_stale_client_eviction_keeps_the_session_and_resumes_it() {
    let mut harness = Harness::signed_in();
    harness.submit(1, Command::Ping);
    let ping = harness.sent_one();
    harness.feed(&eviction(EvictionReason::StaleClient));
    assert_eq!(harness.events(), [Output::CloseTransport]);
    assert!(harness.conn.is_signed_in());

    harness.tick(Duration::from_secs(1));
    harness.connect();
    let bind = harness.sent_one();
    assert_eq!(bind.code(), BIND_SESSION_CODE);
    harness.feed(&session_reply(bind.request(), SESSION));
    assert_eq!(harness.events(), [Output::Resumed { session: SESSION }]);
    // A read is not a mutation, so it goes again with its frozen id.
    let resent = harness.sent_one();
    assert_eq!(resent.request(), ping.request());
}

#[test]
fn a_lost_reply_resumes_the_session_and_resends_the_same_frame() {
    let mut harness = Harness::signed_in();
    harness.submit(1, create_stream());
    let create = harness.sent_one();
    harness.disconnect();
    harness.tick(Duration::from_secs(2));
    assert_eq!(harness.completions().len(), 0);

    harness.connect();
    let bind = harness.sent_one();
    assert_eq!(bind.code(), BIND_SESSION_CODE);
    harness.feed(&session_reply(bind.request(), SESSION));
    let resent = harness.sent_one();
    assert_eq!(resent.request(), create.request());
    assert_eq!(resent.body, create.body);

    // Only a committed result proves the uncertain write.
    harness.feed(&committed(&resent.header));
    assert_eq!(
        harness.completions()[0].result,
        Ok(Reply::Raw(Bytes::new()))
    );
}

#[test]
fn given_committed_lifecycle_refusal_when_retried_should_use_a_fresh_request_id() {
    let mut harness = Harness::signed_in();
    harness.submit(1, create_stream());
    let first = harness.sent_one();
    harness.feed(&refusal(first.request(), 88));
    harness.tick(Duration::from_secs(1));
    let second = harness.sent_one();
    assert_ne!(second.request(), first.request());
    assert_eq!(second.body, first.body);
}

#[test]
fn a_not_committed_refusal_resends_the_same_frame() {
    let mut harness = Harness::signed_in();
    harness.submit(1, create_stream());
    let first = harness.sent_one();
    harness.feed(&refusal(first.request(), 57));
    harness.tick(Duration::from_secs(1));
    let second = harness.sent_one();
    assert_eq!(second.request(), first.request());
}

#[test]
fn a_session_reset_does_not_replay_an_uncertain_write() {
    let mut harness = Harness::signed_in();
    harness.submit(1, create_stream());
    let create = harness.sent_one();
    harness.disconnect();
    harness.tick(Duration::from_secs(2));

    harness.connect();
    let bind = harness.sent_one();
    harness.feed(&refusal(bind.request(), 40));
    assert_eq!(harness.events(), [Output::SessionReset]);
    harness.tick(Duration::from_secs(2));
    assert_eq!(harness.sent().len(), 0);
    assert_eq!(
        harness.completions()[0].result,
        Err(ClientError::OperationUncertain(create.request()))
    );
}

#[test]
fn an_uncertain_write_blocks_its_lane_until_resolved() {
    let mut harness = Harness::signed_in();
    harness.submit_with(1, create_stream(), Some(Duration::from_secs(1)));
    let create = harness.sent_one();
    harness.feed(&refusal(create.request(), 57));
    harness.tick(Duration::from_secs(2));
    assert_eq!(
        harness.completions()[0].result,
        Err(ClientError::OperationUncertain(create.request()))
    );

    harness.submit(2, create_stream());
    assert_eq!(
        harness.completions()[0].result,
        Err(ClientError::LaneBlocked(create.request()))
    );

    harness.submit(3, Command::Resolve(create.request()));
    let replay = harness.sent_one();
    assert_eq!(replay.request(), create.request());
    harness.feed(&committed(&replay.header));
    assert_eq!(harness.completions()[0].result, Ok(Reply::Resolved));

    harness.submit(4, Command::Resolve(create.request()));
    assert_eq!(harness.completions()[0].result, Ok(Reply::Resolved));
    harness.submit(5, create_stream());
    assert_eq!(harness.sent_one().code(), CREATE_STREAM_CODE);
}

#[test]
fn an_abandoned_operation_frees_its_lane() {
    let mut harness = Harness::signed_in();
    harness.submit_with(1, create_stream(), Some(Duration::from_secs(1)));
    let create = harness.sent_one();
    harness.feed(&refusal(create.request(), 57));
    harness.tick(Duration::from_secs(2));
    harness.completions();
    harness.conn.abandon(create.request()).unwrap();
    harness.submit(2, create_stream());
    assert_eq!(harness.sent_one().code(), CREATE_STREAM_CODE);
}

#[test]
fn a_partition_send_looks_up_its_route_and_stamps_the_incarnation() {
    let mut harness = Harness::signed_in();
    harness.submit(1, send(b"one"));
    let lookup = harness.sent_one();
    assert_eq!(lookup.code(), GET_PARTITION_ROUTE_CODE);
    harness.feed(&route_reply(lookup.request()));
    let send_frame = harness.sent_one();
    assert_eq!(send_frame.code(), SEND_MESSAGES_CODE);
    assert_eq!(send_frame.header.partition_incarnation, INCARNATION);
    harness.feed(&send_reply(send_frame.request()));
    assert_eq!(harness.completions()[0].result, Ok(Reply::Sent));

    // The route is cached, so the next send goes at once.
    harness.submit(2, send(b"two"));
    assert_eq!(harness.sent_one().code(), SEND_MESSAGES_CODE);
}

#[test]
fn writes_to_one_partition_wait_for_each_other() {
    let mut harness = Harness::signed_in();
    harness.submit(1, send(b"one"));
    let lookup = harness.sent_one();
    harness.submit(2, send(b"two"));
    assert_eq!(harness.sent().len(), 0);
    harness.feed(&route_reply(lookup.request()));
    let first = harness.sent_one();
    harness.feed(&send_reply(first.request()));
    let second = harness.sent_one();
    assert_ne!(first.request(), second.request());
}

#[test]
fn a_silent_connection_sends_a_heartbeat() {
    let mut harness = Harness::signed_in();
    harness.tick(Duration::from_secs(4));
    assert_eq!(harness.sent().len(), 0);
    harness.tick(Duration::from_secs(2));
    let ping = harness.sent_one();
    assert_eq!(ping.code(), PING_CODE);
    harness.feed(&reply(ping.request(), b""));
    assert_eq!(harness.completions().len(), 0);
    assert_eq!(harness.events().len(), 0);
}

#[test]
fn close_logs_out_and_closes_the_transport() {
    let mut harness = Harness::signed_in();
    harness.submit(1, Command::Ping);
    harness.sent();
    harness.conn.close().unwrap();
    assert_eq!(harness.completions()[0].result, Err(ClientError::Closed));
    let logout = harness.sent_one();
    assert_eq!(logout.code(), LOGOUT_USER_CODE);
    harness.feed(&frame(
        ReplyHeader {
            command: FrameCommand::Reply,
            operation: Operation::Logout,
            request: logout.request(),
            ..Default::default()
        },
        &[],
    ));
    assert_eq!(harness.events(), [Output::Closed, Output::CloseTransport]);
    assert!(matches!(
        harness.conn.handle_write(Submit {
            handle: RequestHandle(2),
            command: Command::Ping,
            timeout: None,
        }),
        Err(ClientError::Closed)
    ));
}

#[test]
fn a_frame_above_the_limit_closes_the_transport() {
    let mut harness = Harness::new();
    harness.conn = Connection::new(
        Config {
            max_frame_size: HEADER_SIZE + 8,
            ..Config::default()
        },
        Counter::default(),
    )
    .unwrap();
    harness.connect();
    harness.submit(1, Command::Ping);
    harness.sent_one();
    harness.feed(&reply(0, &[0; 9]));
    assert_eq!(harness.events(), [Output::CloseTransport]);
}

#[test]
fn the_machine_asks_to_wake_for_the_next_deadline() {
    let mut harness = Harness::signed_in();
    let heartbeat = harness.conn.poll_timeout().unwrap();
    assert_eq!(heartbeat, harness.now + Duration::from_secs(5));
    harness.submit_with(1, Command::Ping, Some(Duration::from_secs(1)));
    harness.sent();
    assert_eq!(
        harness.conn.poll_timeout(),
        Some(harness.now + Duration::from_secs(1))
    );
}
