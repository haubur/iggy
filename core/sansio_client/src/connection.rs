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

//! The connection state machine.
//!
//! One [`Connection`] runs one logical client session over one transport
//! connection at a time. It ports the call sequence of
//! `core/sdk/src/driver.rs` (route, lane, attempts, retries, deadline), the
//! request matching of `core/sdk/src/links/mux.rs`, and the bind and
//! keepalive of `core/sdk/src/links/replicas.rs`. It never reads a socket
//! or a clock.

use crate::command::{Command, Encoded, Kind, Payload, Reply, decode_reply as decode_typed};
use crate::config::Config;
use crate::entropy::{self, Entropy};
use crate::error::ClientError;
use crate::framer::{Framer, InFrame};
use crate::header::{
    Frame, UNBOUND_REQUEST_ID, decode_eviction, decode_reply, encode_request_header,
    metadata_commit, peek_command, reply_request_id,
};
use crate::lane::{CallId, Entry, LaneKey, Lanes, Retained, is_mutation};
use crate::retry::backoff;
use crate::route::{PartitionKey, RouteCache};
use crate::session::Session;
use crate::time::Instant;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use bytes::Bytes;
use core::time::Duration;
use iggy_binary_protocol::codes::{
    BIND_SESSION_CODE, GET_PARTITION_ROUTE_CODE, LOGOUT_USER_CODE, PING_CODE,
};
use iggy_binary_protocol::consensus::operation_for_code;
use iggy_binary_protocol::consensus::{Command as FrameCommand, EvictionReason, HEADER_SIZE};
use iggy_binary_protocol::requests::system::{BindSessionRequest, PingRequest};
use iggy_binary_protocol::requests::users::LogoutUserRequest;
use iggy_binary_protocol::responses::messages::PartitionRouteResponse;
use iggy_binary_protocol::responses::users::LoginRegisterResponse;
use iggy_binary_protocol::{
    ClientVersionInfo, IGGY_PROTOCOL_VERSION, Operation, PartitionContext, RequestHeader,
    WireDecode, WireEncode, WireName,
};

/// The handle that the application gives a request, to match its
/// [`Completion`]. The machine does not read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestHandle(pub u64);

/// A new request of the application.
#[derive(Debug, Clone)]
pub struct Submit {
    pub handle: RequestHandle,
    pub command: Command,
    /// The deadline of the request, from now. `None` takes
    /// [`Config::request_timeout`].
    pub timeout: Option<Duration>,
}

/// One finished request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub handle: RequestHandle,
    pub result: Result<Reply, ClientError>,
}

/// One frame to write. The driver writes the header, then the body, and
/// never interleaves two frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmit {
    pub header: [u8; HEADER_SIZE],
    pub body: Bytes,
}

/// Transport state from the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A new transport connection is open. The bytes before it are gone.
    Connected,
    /// The transport connection closed.
    Disconnected,
}

/// Session events for the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// A login bound a new session.
    SignedIn {
        user_id: u32,
        session: u64,
        server_protocol_version: u32,
    },
    /// A new connection resumed the bound session.
    Resumed { session: u64 },
    /// The server no longer knows the session. The application signs in
    /// again.
    SessionReset,
    /// The server ended the session.
    Evicted {
        reason: EvictionReason,
        server_protocol_version: u32,
        server_protocol_version_min: u32,
    },
    /// The driver closes the transport connection, and dials again if it
    /// wants to go on.
    CloseTransport,
    /// The connection is closed for good.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    Down,
    /// A `BIND_SESSION` is on the wire. Calls wait for it.
    Binding,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Open,
    /// `close()` sent a `Logout` and waits for its reply.
    LoggingOut,
    Closed,
}

/// A frame and the session fence it was made under.
#[derive(Debug, Clone)]
struct Frozen {
    frame: Frame,
    fence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// The next attempt can start.
    Ready,
    /// The next attempt starts at this time.
    Paused(Instant),
    /// A route lookup for the partition is on the wire.
    WaitRoute,
    /// The frame with this request id is on the wire.
    InFlight,
}

/// One request of the application and what its attempts saw so far. The
/// flags mirror `Call` in `core/sdk/src/driver.rs`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct Call {
    handle: RequestHandle,
    kind: Kind,
    code: u32,
    payload: Payload,
    partition: Option<crate::command::Partition>,
    deadline: Instant,
    /// The call started with a bound session, so it never goes unbound.
    requires_session: bool,
    state: State,
    frozen: Option<Frozen>,
    /// The route that the frame took its context from.
    route: Option<PartitionRouteResponse>,
    /// The lane that the call holds.
    lane: Option<LaneKey>,
    /// The retained operation that a resolve replays.
    resume: Option<u64>,
    /// An attempt can have taken effect, so the call never freezes again.
    uncertain: bool,
    /// The error at the deadline, unless an attempt can have taken effect.
    refusal: ClientError,
    retries: u32,
    /// The last attempt got `NotPrimary`. Another one in a row pauses first.
    not_primary: bool,
    /// A send that got `HistoryUnavailable` started over once.
    restarted: bool,
}

impl Call {
    fn progress_poll(&self) -> bool {
        self.partition
            .as_ref()
            .is_some_and(|partition| partition.progress_poll)
    }

    fn partition_id(&self) -> u32 {
        self.partition
            .as_ref()
            .map_or(0, |partition| partition.key.partition_id())
    }

    fn op(&self) -> Option<u64> {
        self.frozen
            .as_ref()
            .map(|frozen| frozen.frame.header.request)
    }

    /// Whether a send that got `HistoryUnavailable` starts over under the
    /// next route: only once, and only while no attempt took effect.
    fn restarts_on_new_history(&self) -> bool {
        self.kind == Kind::Send && !self.uncertain && !self.restarted && self.resume.is_none()
    }
}

/// What a frame on the wire belongs to.
#[derive(Debug, Clone)]
enum Exchange {
    Call(CallId),
    Lookup(PartitionKey),
    Heartbeat,
    Bind,
    Logout,
}

#[derive(Debug, Clone)]
struct OnWire {
    exchange: Exchange,
    request: RequestHeader,
    deadline: Instant,
}

/// What the next attempt of a call does.
enum Step {
    /// The frame goes on the wire.
    Go,
    /// The call waits. `true` if something moved that can unblock another
    /// call.
    Park(bool),
    End(ClientError),
    Expire,
}

/// Whether a call can go on in its lane.
enum LaneStep {
    Entered(LaneKey),
    Wait,
    /// The call takes no lane.
    Free,
}

/// What a call does after an attempt that did not end it.
enum Next {
    Again,
    Pause,
    Fail(ClientError),
}

/// The sans-IO Iggy client connection.
///
/// The driver feeds it bytes, transport events, requests and the time, and
/// takes frames to write, completions, events and the next wake time. See
/// the crate docs for the driver loop.
#[derive(Debug)]
pub struct Connection<E: Entropy> {
    config: Config,
    entropy: E,
    version_info: ClientVersionInfo,
    session: Session,
    framer: Framer,
    link: Link,
    lifecycle: Lifecycle,
    calls: BTreeMap<CallId, Call>,
    next_call: CallId,
    /// The frames on the wire, by request id.
    wire: BTreeMap<u64, OnWire>,
    /// The partitions with a route lookup on the wire.
    lookups: BTreeSet<PartitionKey>,
    routes: RouteCache,
    lanes: Lanes,
    now: Instant,
    last_transmit: Instant,
    transmits: VecDeque<Transmit>,
    completions: VecDeque<Completion>,
    events: VecDeque<Output>,
}

impl<E: Entropy> Connection<E> {
    /// A connection that is not yet connected and has no session.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::Wire` if the SDK name or version in `config`
    /// is not 1 to 255 bytes long.
    pub fn new(config: Config, mut entropy: E) -> Result<Self, ClientError> {
        let version_info = ClientVersionInfo {
            protocol_version: IGGY_PROTOCOL_VERSION,
            sdk_name: WireName::new(config.sdk_name.clone())?,
            sdk_version: WireName::new(config.sdk_version.clone())?,
        };
        let session = Session::new(&mut entropy);
        Ok(Self {
            routes: RouteCache::new(config.max_cached_routes),
            lanes: Lanes::new(config.max_proved_outcomes),
            config,
            entropy,
            version_info,
            session,
            framer: Framer::default(),
            link: Link::Down,
            lifecycle: Lifecycle::Open,
            calls: BTreeMap::new(),
            next_call: 0,
            wire: BTreeMap::new(),
            lookups: BTreeSet::new(),
            now: Instant::default(),
            last_transmit: Instant::default(),
            transmits: VecDeque::new(),
            completions: VecDeque::new(),
            events: VecDeque::new(),
        })
    }

    /// Whether a session is bound.
    pub const fn is_signed_in(&self) -> bool {
        self.session.is_bound()
    }

    /// The client id of the current session identity.
    pub const fn client_id(&self) -> u128 {
        self.session.client_id()
    }

    /// Frees the lane of the retained operation `op`. Its outcome stays
    /// unknown.
    ///
    /// # Errors
    ///
    /// Returns `OperationNotRetained` if no lane retains `op`.
    pub fn abandon(&mut self, op: u64) -> Result<(), ClientError> {
        self.lanes.abandon(op)?;
        self.pump();
        Ok(())
    }

    // ----- inputs -------------------------------------------------------

    fn submit(&mut self, submit: Submit) -> Result<(), ClientError> {
        if self.lifecycle != Lifecycle::Open {
            return Err(ClientError::Closed);
        }
        if self.calls.values().any(|call| call.handle == submit.handle) {
            return Err(ClientError::DuplicateHandle);
        }
        if self.calls.len() >= self.config.max_in_flight + self.config.max_queued {
            return Err(ClientError::QueueFull);
        }
        let deadline = self.now + submit.timeout.unwrap_or(self.config.request_timeout);
        let call = match submit.command {
            Command::Resolve(op) => match self.resolve_call(submit.handle, op, deadline) {
                Some(call) => call,
                None => return Ok(()),
            },
            command => {
                let Encoded {
                    kind,
                    code,
                    payload,
                    partition,
                } = command.encode()?.ok_or(ClientError::Unsupported)?;
                Call {
                    handle: submit.handle,
                    kind,
                    code,
                    payload,
                    partition,
                    deadline,
                    requires_session: self.session.is_bound(),
                    state: State::Ready,
                    frozen: None,
                    route: None,
                    lane: None,
                    resume: None,
                    uncertain: false,
                    refusal: ClientError::TransientNotAccepted,
                    retries: 0,
                    not_primary: false,
                    restarted: false,
                }
            }
        };
        let id = self.next_call;
        self.next_call += 1;
        self.calls.insert(id, call);
        self.pump();
        Ok(())
    }

    /// The call that replays the retained operation `op`. If no lane
    /// retains it, the outcome that a replay proved completes the request
    /// at once, and the result is `None`.
    fn resolve_call(&mut self, handle: RequestHandle, op: u64, deadline: Instant) -> Option<Call> {
        let Some(retained) = self.lanes.retained(op) else {
            let result = match self.lanes.proved_outcome(op) {
                Some(Ok(())) => Ok(Reply::Resolved),
                Some(Err(error)) => Err(error),
                None => Err(ClientError::OperationNotRetained(op)),
            };
            self.completions.push_back(Completion { handle, result });
            return None;
        };
        let Retained {
            code,
            frame,
            partition,
        } = retained;
        Some(Call {
            handle,
            kind: Kind::Resolve,
            code,
            payload: Payload::Bytes(frame.body.clone()),
            partition: partition.map(|key| crate::command::Partition {
                key,
                progress_poll: false,
            }),
            deadline,
            requires_session: true,
            state: State::Ready,
            frozen: Some(Frozen {
                frame,
                fence: self.session.fence(),
            }),
            route: None,
            lane: None,
            resume: Some(op),
            uncertain: true,
            refusal: ClientError::OperationUncertain(op),
            retries: 0,
            not_primary: false,
            restarted: false,
        })
    }

    fn read(&mut self, bytes: &[u8]) {
        if self.link == Link::Down || self.lifecycle == Lifecycle::Closed {
            return;
        }
        self.framer.push(bytes);
        loop {
            match self.framer.next_frame(self.config.max_frame_size) {
                Ok(Some(frame)) => self.dispatch(frame),
                Ok(None) => break,
                Err(_) => {
                    self.lose_link(true);
                    break;
                }
            }
            if self.link == Link::Down {
                break;
            }
        }
        self.pump();
    }

    fn event(&mut self, input: Input) {
        match input {
            Input::Connected => {
                if self.lifecycle != Lifecycle::Open {
                    return;
                }
                self.lose_link(false);
                self.framer.clear();
                self.last_transmit = self.now;
                if self.session.is_bound() {
                    self.link = Link::Binding;
                    self.send_bind();
                } else {
                    self.link = Link::Up;
                }
            }
            Input::Disconnected => {
                self.lose_link(false);
                if self.lifecycle == Lifecycle::LoggingOut {
                    self.finish_close();
                }
            }
        }
        self.pump();
    }

    fn timeout(&mut self, now: Instant) {
        self.now = self.now.max(now);
        let due: Vec<u64> = self
            .wire
            .iter()
            .filter(|(_, on_wire)| self.now >= on_wire.deadline)
            .map(|(request, _)| *request)
            .collect();
        let mut close = false;
        for request in due {
            let Some(on_wire) = self.wire.remove(&request) else {
                continue;
            };
            // A late reply with id 0 would answer the next unbound request.
            close |= request == UNBOUND_REQUEST_ID;
            match on_wire.exchange {
                Exchange::Call(id) => self.settle_by_id(id, Err(ClientError::Timeout)),
                Exchange::Lookup(key) => {
                    self.lookups.remove(&key);
                    self.route_outcome(&key, &Err(ClientError::Timeout));
                }
                Exchange::Heartbeat | Exchange::Bind => close = true,
                Exchange::Logout => self.finish_close(),
            }
        }
        if close {
            self.lose_link(true);
        }
        self.pump();
    }

    fn close_connection(&mut self) {
        if self.lifecycle != Lifecycle::Open {
            return;
        }
        let calls = core::mem::take(&mut self.calls);
        for call in calls.into_values() {
            self.completions.push_back(Completion {
                handle: call.handle,
                result: Err(ClientError::Closed),
            });
        }
        self.lanes.clear();
        self.lookups.clear();
        self.wire.clear();
        if self.link == Link::Up && self.session.is_bound() {
            let body = LogoutUserRequest.to_bytes();
            if let Ok(header) = encode_request_header(&mut self.session, LOGOUT_USER_CODE, &body) {
                self.lifecycle = Lifecycle::LoggingOut;
                let deadline = self.now + self.config.request_timeout;
                self.transmit(Exchange::Logout, Frame { header, body }, deadline);
                return;
            }
        }
        self.finish_close();
    }

    fn finish_close(&mut self) {
        if self.lifecycle == Lifecycle::Closed {
            return;
        }
        self.lifecycle = Lifecycle::Closed;
        if self.session.is_bound() {
            self.session.reset(&mut self.entropy);
        }
        self.wire.clear();
        self.framer.clear();
        self.events.push_back(Output::Closed);
        if self.link != Link::Down {
            self.link = Link::Down;
            self.events.push_back(Output::CloseTransport);
        }
    }

    // ----- frames from the server ---------------------------------------

    fn dispatch(&mut self, frame: InFrame) {
        match peek_command(&frame.header) {
            FrameCommand::Reply => {
                let request = reply_request_id(&frame.header);
                let Some(on_wire) = self.wire.remove(&request) else {
                    // It answers a request that timed out.
                    return;
                };
                if let Some(commit) = metadata_commit(&frame.header) {
                    self.session.observe_metadata_commit(commit);
                }
                match on_wire.exchange {
                    Exchange::Call(id) => {
                        self.on_call_reply(id, &on_wire.request, &frame.header, frame.body);
                    }
                    Exchange::Lookup(key) => {
                        self.on_lookup_reply(&key, &on_wire.request, &frame.header, frame.body);
                    }
                    Exchange::Heartbeat => {
                        let healthy = decode_reply(&on_wire.request, &frame.header, frame.body)
                            .is_ok_and(|outcome| outcome.result.is_ok());
                        if !healthy {
                            self.lose_link(true);
                        }
                    }
                    Exchange::Bind => {
                        self.on_bind_reply(&on_wire.request, &frame.header, frame.body);
                    }
                    Exchange::Logout => self.finish_close(),
                }
            }
            FrameCommand::Eviction => self.on_eviction(&frame.header),
            _ => self.lose_link(true),
        }
    }

    fn on_call_reply(
        &mut self,
        id: CallId,
        request: &RequestHeader,
        header: &[u8; HEADER_SIZE],
        body: Bytes,
    ) {
        let Some(mut call) = self.calls.remove(&id) else {
            return;
        };
        call.state = State::Ready;
        let Ok(outcome) = decode_reply(request, header, body) else {
            // A reply that does not match its request breaks the stream.
            self.settle(id, call, Err(ClientError::Disconnected));
            self.lose_link(true);
            return;
        };
        let bound = self.session.is_bound();
        let detached = matches!(outcome.result, Err(ClientError::StaleClient))
            || (bound && matches!(outcome.result, Err(ClientError::Unauthenticated)));
        if outcome.committed && call.uncertain {
            // The reply proves the outcome of a frame that could have
            // taken effect before.
            match outcome.result {
                Ok(body) => self.succeed(id, &call, body),
                Err(error) => self.finish(id, &call, Err(error)),
            }
        } else {
            let result = if bound
                && request.operation == Operation::NonReplicated
                && call.kind != Kind::SignIn
                && outcome.result == Err(ClientError::Unauthenticated)
            {
                // A detached connection refused the read before admission.
                Err(ClientError::TransientNotAccepted)
            } else if call.uncertain
                && !matches!(
                    request.operation,
                    Operation::NonReplicated | Operation::Register | Operation::Logout
                )
                && outcome.result.is_ok()
            {
                // Only a committed result proves an uncertain write.
                Err(ClientError::TransientNotCommitted)
            } else {
                outcome.result
            };
            self.settle(id, call, result);
        }
        if detached {
            self.lose_link(true);
        }
    }

    fn on_lookup_reply(
        &mut self,
        key: &PartitionKey,
        request: &RequestHeader,
        header: &[u8; HEADER_SIZE],
        body: Bytes,
    ) {
        self.lookups.remove(key);
        let Ok(outcome) = decode_reply(request, header, body) else {
            self.route_outcome(key, &Err(ClientError::Disconnected));
            self.lose_link(true);
            return;
        };
        let bound = self.session.is_bound();
        let outcome = match outcome.result {
            Ok(body) => PartitionRouteResponse::decode_from(&body)
                .map_err(|_| ClientError::InvalidReply)
                .and_then(|answer| {
                    self.routes
                        .merge(key, answer, self.session.fence())
                        .map(drop)
                        // The answer is older than the cached route.
                        .ok_or(ClientError::TransientNotAccepted)
                }),
            Err(ClientError::Unauthenticated) if bound => {
                self.route_outcome(key, &Err(ClientError::TransientNotAccepted));
                self.lose_link(true);
                return;
            }
            Err(error) => Err(error),
        };
        self.route_outcome(key, &outcome);
    }

    fn on_bind_reply(&mut self, request: &RequestHeader, header: &[u8; HEADER_SIZE], body: Bytes) {
        let Ok(outcome) = decode_reply(request, header, body) else {
            self.lose_link(true);
            return;
        };
        match outcome.result {
            Ok(body) => {
                let expected = self.session.session().unwrap_or(0);
                match LoginRegisterResponse::decode_from(&body) {
                    Ok(bound) if bound.session != 0 && bound.session == expected => {
                        self.link = Link::Up;
                        self.events.push_back(Output::Resumed {
                            session: bound.session,
                        });
                    }
                    _ => self.lose_link(true),
                }
            }
            // The server no longer knows the session.
            Err(ClientError::Unauthenticated | ClientError::TransientNotCommitted) => {
                self.reset_session(true);
                self.link = Link::Up;
            }
            // This node is behind the session. Another one can be ahead.
            Err(_) => self.lose_link(true),
        }
    }

    fn on_eviction(&mut self, header: &[u8; HEADER_SIZE]) {
        let error = decode_eviction(header);
        let ClientError::Evicted {
            reason,
            server_protocol_version,
            server_protocol_version_min,
        } = error
        else {
            return;
        };
        match reason {
            // Only the binding of this connection ended. The next bind
            // resumes the session.
            EvictionReason::StaleClient => self.lose_link(true),
            // The next bind decides whether the session still exists.
            EvictionReason::NoSession if self.session.is_bound() => self.lose_link(true),
            _ => {
                self.events.push_back(Output::Evicted {
                    reason,
                    server_protocol_version,
                    server_protocol_version_min,
                });
                let calls = core::mem::take(&mut self.calls);
                for (id, call) in calls {
                    self.finish(id, &call, Err(error.clone()));
                }
                if self.session.is_bound() {
                    self.reset_session(false);
                }
                self.lose_link(true);
            }
        }
    }

    // ----- the attempts of a call ----------------------------------------

    /// Starts every attempt that can start, in arrival order, until no call
    /// moves.
    fn pump(&mut self) {
        loop {
            let mut moved = false;
            let ids: Vec<CallId> = self.calls.keys().copied().collect();
            for id in ids {
                if let Some(call) = self.calls.remove(&id) {
                    moved |= self.advance(id, call);
                }
            }
            if !moved {
                break;
            }
        }
        self.heartbeat();
    }

    /// Takes `call` one step on. Returns whether anything moved that can
    /// unblock another call. The call goes back into the table unless it
    /// completed.
    fn advance(&mut self, id: CallId, mut call: Call) -> bool {
        match self.prepare(id, &mut call) {
            Step::Go => {
                let Some(frozen) = &call.frozen else {
                    return false;
                };
                let frame = frozen.frame.clone();
                let deadline = call.deadline;
                call.state = State::InFlight;
                self.calls.insert(id, call);
                self.transmit(Exchange::Call(id), frame, deadline);
                true
            }
            Step::Park(moved) => {
                self.calls.insert(id, call);
                moved
            }
            Step::End(error) => {
                self.finish(id, &call, Err(error));
                true
            }
            Step::Expire => {
                self.expire(id, call);
                true
            }
        }
    }

    /// Runs the gates of the next attempt of `call`, in the order of
    /// `attempt` in `core/sdk/src/driver.rs`, and freezes its frame.
    fn prepare(&mut self, id: CallId, call: &mut Call) -> Step {
        match call.state {
            State::InFlight => return Step::Park(false),
            _ if self.now >= call.deadline => return Step::Expire,
            State::WaitRoute => return Step::Park(false),
            State::Paused(until) if self.now < until => return Step::Park(false),
            State::Ready | State::Paused(_) => call.state = State::Ready,
        }
        if self.link != Link::Up {
            if call.refusal == ClientError::TransientNotAccepted && !call.uncertain {
                call.refusal = ClientError::Disconnected;
            }
            return Step::Park(false);
        }
        if let Some(frozen) = &call.frozen
            && frozen.fence != self.session.fence()
        {
            // A replaced session can never apply the frame. An uncertain
            // call ends with its uncertainty, any other call freezes again.
            if call.uncertain {
                return Step::End(ClientError::OperationUncertain(frozen.frame.header.request));
            }
            call.frozen = None;
        }
        if call.frozen.is_none() {
            if let Some(step) = self.session_gate(call) {
                return step;
            }
            if let Some(step) = self.route_gate(call) {
                return step;
            }
        }
        if call.lane.is_none() {
            match self.enter_lane(id, call) {
                Ok(LaneStep::Entered(key)) => call.lane = Some(key),
                Ok(LaneStep::Wait) => return Step::Park(false),
                Ok(LaneStep::Free) => {}
                Err(error) => return Step::End(error),
            }
        }
        let unbound = call.frozen.as_ref().map_or_else(
            || {
                call.kind == Kind::SignIn
                    || (!self.session.is_bound()
                        && operation_for_code(call.code) == Operation::NonReplicated)
            },
            |frozen| frozen.frame.is_unbound(),
        );
        if !self.can_send(unbound) {
            return Step::Park(false);
        }
        if call.frozen.is_none() {
            match self.freeze(call) {
                Ok(frozen) => call.frozen = Some(frozen),
                Err(error) => return Step::End(error),
            }
        }
        Step::Go
    }

    /// A login needs an unbound session. Any other call that needs a
    /// session waits for a login on its way, or fails.
    fn session_gate(&self, call: &Call) -> Option<Step> {
        if call.kind == Kind::SignIn {
            return self
                .session
                .is_bound()
                .then_some(Step::End(ClientError::AlreadySignedIn));
        }
        if self.session.is_bound() {
            return None;
        }
        if self.calls.values().any(|other| other.kind == Kind::SignIn) {
            return Some(Step::Park(false));
        }
        (call.requires_session || operation_for_code(call.code) != Operation::NonReplicated)
            .then_some(Step::End(ClientError::Unauthenticated))
    }

    /// A partition call takes the cached route, or waits for a lookup.
    fn route_gate(&mut self, call: &mut Call) -> Option<Step> {
        let key = call.partition.as_ref()?.key.clone();
        let fence = self.session.fence();
        let watermark = self.session.metadata_watermark();
        if let Some(route) = self.routes.usable(&key, fence, watermark) {
            call.route = Some(route);
            return None;
        }
        let moved = self.lookup(&key);
        if moved || self.lookups.contains(&key) {
            call.state = State::WaitRoute;
        }
        Some(Step::Park(moved))
    }

    /// Lets `call` into its lane. `Free` for a call without a lane.
    fn enter_lane(&mut self, id: CallId, call: &Call) -> Result<LaneStep, ClientError> {
        if let Some(op) = call.resume {
            return match self.lanes.resume(op, id) {
                Some((key, Entry::Entered, _)) => Ok(LaneStep::Entered(key)),
                Some((_, Entry::Wait, _)) => Ok(LaneStep::Wait),
                None => Err(self
                    .lanes
                    .proved_outcome(op)
                    .and_then(Result::err)
                    .unwrap_or(ClientError::OperationNotRetained(op))),
            };
        }
        let context = self.context(call);
        let key = if call.progress_poll() {
            Some(LaneKey::partition(&context, call.partition_id()))
        } else {
            LaneKey::of(call.code, &context, call.partition_id())
        };
        let Some(key) = key else {
            return Ok(LaneStep::Free);
        };
        Ok(match self.lanes.enter(key, id)? {
            Entry::Entered => LaneStep::Entered(key),
            Entry::Wait => LaneStep::Wait,
        })
    }

    /// The partition context that a new frame of `call` carries.
    fn context(&self, call: &Call) -> PartitionContext {
        match (&call.partition, &call.route) {
            (Some(_), Some(route)) => route.context(),
            _ if operation_for_code(call.code) == Operation::NonReplicated => self.read_context(),
            _ => PartitionContext::default(),
        }
    }

    /// A read waits until the server applied the metadata that the session
    /// saw.
    fn read_context(&self) -> PartitionContext {
        PartitionContext {
            metadata_op: self.session.metadata_watermark(),
            ..PartitionContext::default()
        }
    }

    /// Takes the next request id and freezes the frame of `call`.
    fn freeze(&mut self, call: &Call) -> Result<Frozen, ClientError> {
        let body = match &call.payload {
            Payload::Bytes(body) => body.clone(),
            Payload::SignIn(sign_in) => {
                sign_in.encode(self.version_info.clone(), self.session.bind_secret())
            }
        };
        let context = self.context(call);
        let mut header = encode_request_header(&mut self.session, call.code, &body)?;
        context.stamp(&mut header);
        Ok(Frozen {
            frame: Frame { header, body },
            fence: self.session.fence(),
        })
    }

    /// Sends a route lookup for `key`, unless one is on the wire or no
    /// frame can go. Returns whether it sent one.
    fn lookup(&mut self, key: &PartitionKey) -> bool {
        if self.lookups.contains(key) || !self.can_send(!self.session.is_bound()) {
            return false;
        }
        let body = key.lookup_body();
        let mut header =
            match encode_request_header(&mut self.session, GET_PARTITION_ROUTE_CODE, &body) {
                Ok(header) => header,
                Err(error) => {
                    self.route_outcome(key, &Err(error));
                    return true;
                }
            };
        self.read_context().stamp(&mut header);
        self.lookups.insert(key.clone());
        let deadline = self.now + self.config.request_timeout;
        self.transmit(
            Exchange::Lookup(key.clone()),
            Frame { header, body },
            deadline,
        );
        true
    }

    /// Applies the outcome of a route lookup to the calls that wait for it.
    fn route_outcome(&mut self, key: &PartitionKey, outcome: &Result<(), ClientError>) {
        let waiting: Vec<CallId> = self
            .calls
            .iter()
            .filter(|(_, call)| {
                call.state == State::WaitRoute
                    && call.partition.as_ref().is_some_and(|p| p.key == *key)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            let Some(mut call) = self.calls.remove(&id) else {
                continue;
            };
            call.state = State::Ready;
            match outcome {
                Ok(()) => {
                    self.calls.insert(id, call);
                }
                // Another attempt, or the same node later, can answer.
                Err(
                    error @ (ClientError::TransientNotAccepted
                    | ClientError::NotPrimary
                    | ClientError::Disconnected
                    | ClientError::Timeout),
                ) => {
                    call.refusal = error.clone();
                    self.apply(id, call, Next::Pause);
                }
                Err(error) => self.finish(id, &call, Err(error.clone())),
            }
        }
    }

    fn settle_by_id(&mut self, id: CallId, result: Result<Bytes, ClientError>) {
        if let Some(mut call) = self.calls.remove(&id) {
            call.state = State::Ready;
            self.settle(id, call, result);
        }
    }

    /// Reads the outcome of one attempt, as `settle` in
    /// `core/sdk/src/driver.rs` does.
    fn settle(&mut self, id: CallId, mut call: Call, result: Result<Bytes, ClientError>) {
        let error = match result {
            Ok(body) => return self.succeed(id, &call, body),
            Err(error) => error,
        };
        if call.progress_poll()
            && (error == ClientError::TransientNotCommitted || error.is_lost_link())
        {
            // A replay can advance the offset again.
            call.uncertain = true;
            return self.end(id, call, ClientError::TransientNotCommitted);
        }
        let repeated = core::mem::replace(&mut call.not_primary, error == ClientError::NotPrimary);
        let next = match error {
            ClientError::TransientNotCommitted => {
                call.uncertain = true;
                Next::Pause
            }
            ClientError::TransientNotAccepted => {
                call.refusal = error;
                self.invalidate_route(&call, false);
                Next::Pause
            }
            ClientError::NotPrimary => {
                call.refusal = error;
                self.invalidate_route(&call, true);
                if repeated { Next::Pause } else { Next::Again }
            }
            error if error.is_lost_link() => {
                // The frame left, so a write can have taken effect.
                call.uncertain |= call.lane.is_some() && is_mutation(call.code);
                call.refusal = error;
                Next::Pause
            }
            // The refusal spent the request id, so the call starts a new
            // operation in the same lane.
            ClientError::LifecycleBusy if !call.uncertain => {
                call.refusal = error;
                call.frozen = None;
                Next::Pause
            }
            ClientError::HistoryUnavailable => {
                self.invalidate_route(&call, false);
                if call.restarts_on_new_history() {
                    if let Some(key) = call.lane.take() {
                        self.lanes.leave(key, id);
                    }
                    call.frozen = None;
                    call.route = None;
                    call.restarted = true;
                    Next::Again
                } else {
                    Next::Fail(error)
                }
            }
            ClientError::PartitionNotFound | ClientError::PartitionNotOwned => {
                self.invalidate_route(&call, false);
                Next::Fail(error)
            }
            error => Next::Fail(error),
        };
        self.apply(id, call, next);
    }

    fn apply(&mut self, id: CallId, mut call: Call, next: Next) {
        match next {
            Next::Again if self.now < call.deadline => {
                call.state = State::Ready;
                self.calls.insert(id, call);
            }
            Next::Again => self.expire(id, call),
            Next::Pause => {
                let pause = backoff(call.retries, entropy::fraction(&mut self.entropy));
                call.retries = call.retries.saturating_add(1);
                let resume_at = self.now + pause;
                if resume_at >= call.deadline {
                    self.expire(id, call);
                } else {
                    call.state = State::Paused(resume_at);
                    self.calls.insert(id, call);
                }
            }
            Next::Fail(error) => self.end(id, call, error),
        }
    }

    fn invalidate_route(&mut self, call: &Call, not_primary: bool) {
        if let (Some(partition), Some(route)) = (&call.partition, &call.route) {
            let partition_id = partition.key.partition_id();
            if not_primary {
                self.routes.not_primary(route, partition_id);
            } else {
                self.routes.invalidate(route, partition_id);
            }
        }
    }

    /// Ends `call` with the reply body of its last attempt.
    fn succeed(&mut self, id: CallId, call: &Call, body: Bytes) {
        let result = match call.kind {
            Kind::SignIn => self.signed_in(call, &body),
            Kind::Logout => {
                // The committed logout ended the session.
                self.reset_session(false);
                Ok(Reply::LoggedOut)
            }
            kind => decode_typed(kind, body),
        };
        self.finish(id, call, result);
    }

    /// Binds the session of a successful login, at the fence that the login
    /// froze its frame at.
    fn signed_in(&mut self, call: &Call, body: &Bytes) -> Result<Reply, ClientError> {
        let response =
            LoginRegisterResponse::decode_from(body).map_err(|_| ClientError::InvalidReply)?;
        let fence = call.frozen.as_ref().map_or(u64::MAX, |frozen| frozen.fence);
        self.session.bind_at(fence, response.session)?;
        self.events.push_back(Output::SignedIn {
            user_id: response.user_id,
            session: response.session,
            server_protocol_version: response.server_protocol_version,
        });
        Ok(Reply::SignedIn(response))
    }

    /// Ends `call` with `error`. A write that can have taken effect ends
    /// with its uncertainty instead, and its lane retains it.
    fn end(&mut self, id: CallId, mut call: Call, error: ClientError) {
        if !call.uncertain {
            return self.finish(id, &call, Err(error));
        }
        let (Some(op), Some(key)) = (call.op(), call.lane) else {
            return self.finish(id, &call, Err(ClientError::TransientNotCommitted));
        };
        if !is_mutation(call.code) {
            return self.finish(id, &call, Err(ClientError::TransientNotCommitted));
        }
        let current = call
            .frozen
            .as_ref()
            .is_some_and(|frozen| frozen.fence == self.session.fence());
        if current && let Some(frozen) = call.frozen.take() {
            // An operation of a replaced session cannot replay, so it must
            // not block the lane of the new one.
            self.lanes.retain(
                key,
                id,
                Retained {
                    code: call.code,
                    frame: frozen.frame,
                    partition: call.partition.as_ref().map(|p| p.key.clone()),
                },
            );
            call.lane = None;
        }
        self.finish(id, &call, Err(ClientError::OperationUncertain(op)));
    }

    /// Ends `call` at its deadline with its last refusal.
    fn expire(&mut self, id: CallId, mut call: Call) {
        let refusal = core::mem::replace(&mut call.refusal, ClientError::TransientNotAccepted);
        self.end(id, call, refusal);
    }

    /// Completes `call` and frees its lane. A replay records the outcome
    /// that it proved.
    fn finish(&mut self, id: CallId, call: &Call, result: Result<Reply, ClientError>) {
        if let Some(key) = call.lane {
            match call.resume {
                Some(op) => {
                    let proved = match &result {
                        Ok(_) => Some(Ok(())),
                        Err(ClientError::OperationUncertain(_)) => None,
                        Err(error) => Some(Err(error.clone())),
                    };
                    match proved {
                        Some(outcome) => self.lanes.prove(key, id, op, outcome),
                        None => self.lanes.leave(key, id),
                    }
                }
                None => self.lanes.leave(key, id),
            }
        }
        self.completions.push_back(Completion {
            handle: call.handle,
            result,
        });
    }

    // ----- the connection --------------------------------------------------

    fn can_send(&self, unbound: bool) -> bool {
        self.link == Link::Up
            && self.wire.len() < self.config.max_in_flight
            && !(unbound && self.wire.contains_key(&UNBOUND_REQUEST_ID))
    }

    fn transmit(&mut self, exchange: Exchange, frame: Frame, deadline: Instant) {
        self.wire.insert(
            frame.header.request,
            OnWire {
                exchange,
                request: frame.header,
                deadline,
            },
        );
        self.transmits.push_back(Transmit {
            header: frame.header_bytes(),
            body: frame.body,
        });
        self.last_transmit = self.now;
    }

    fn send_bind(&mut self) {
        let Some(identity) = self.session.identity() else {
            self.link = Link::Up;
            return;
        };
        let body = BindSessionRequest {
            version_info: self.version_info.clone(),
            identity,
            bind_secret: self.session.bind_secret(),
        }
        .to_bytes();
        if let Ok(header) = encode_request_header(&mut self.session, BIND_SESSION_CODE, &body) {
            let deadline = self.now + self.config.request_timeout;
            self.transmit(Exchange::Bind, Frame { header, body }, deadline);
        } else {
            // The request counter ran out, so the session cannot go on.
            self.reset_session(true);
            self.link = Link::Up;
        }
    }

    /// Sends a ping after `heartbeat_interval` of silence. Any frame counts
    /// as a sign of life.
    fn heartbeat(&mut self) {
        if self.lifecycle != Lifecycle::Open
            || self.now < self.last_transmit + self.config.heartbeat_interval
            || self
                .wire
                .values()
                .any(|w| matches!(w.exchange, Exchange::Heartbeat))
            || !self.can_send(!self.session.is_bound())
        {
            return;
        }
        let body = PingRequest.to_bytes();
        if let Ok(header) = encode_request_header(&mut self.session, PING_CODE, &body) {
            let deadline = self.now + self.config.request_timeout;
            self.transmit(Exchange::Heartbeat, Frame { header, body }, deadline);
        }
    }

    /// Ends the transport connection. The frames on the wire count as lost:
    /// their calls retry after the next connect, or end at their deadline.
    fn lose_link(&mut self, close: bool) {
        if self.link == Link::Down {
            return;
        }
        if close {
            self.events.push_back(Output::CloseTransport);
        }
        self.link = Link::Down;
        self.framer.clear();
        self.lookups.clear();
        let wire = core::mem::take(&mut self.wire);
        for on_wire in wire.into_values() {
            match on_wire.exchange {
                Exchange::Call(id) => self.settle_by_id(id, Err(ClientError::Disconnected)),
                Exchange::Lookup(key) => self.route_outcome(&key, &Err(ClientError::Disconnected)),
                Exchange::Heartbeat | Exchange::Bind => {}
                Exchange::Logout => self.finish_close(),
            }
        }
    }

    /// Replaces the session identity. Retained operations cannot replay
    /// under the new session, and routes of the old one are unusable.
    fn reset_session(&mut self, announce: bool) {
        self.session.reset(&mut self.entropy);
        self.lanes.drop_retained();
        if announce {
            self.events.push_back(Output::SessionReset);
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let calls = self.calls.values().filter_map(|call| match call.state {
            State::InFlight => None,
            State::Paused(until) => Some(until.min(call.deadline)),
            State::Ready | State::WaitRoute => Some(call.deadline),
        });
        let wire = self.wire.values().map(|on_wire| on_wire.deadline);
        let heartbeat = (self.link == Link::Up && self.lifecycle == Lifecycle::Open)
            .then(|| self.last_transmit + self.config.heartbeat_interval);
        calls.chain(wire).chain(heartbeat).min()
    }
}

impl<E: Entropy> sansio::Protocol<Bytes, Submit, Input> for Connection<E> {
    type Rout = Completion;
    type Wout = Transmit;
    type Eout = Output;
    type Error = ClientError;
    type Time = Instant;

    /// Takes bytes that the driver read from the transport, in chunks of
    /// any size.
    fn handle_read(&mut self, msg: Bytes) -> Result<(), Self::Error> {
        self.read(&msg);
        Ok(())
    }

    /// The next finished request.
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.completions.pop_front()
    }

    /// Takes a new request of the application.
    ///
    /// # Errors
    ///
    /// Returns `Closed` after `close`, `DuplicateHandle`, `QueueFull`, or
    /// the encode error of the command.
    fn handle_write(&mut self, msg: Submit) -> Result<(), Self::Error> {
        self.submit(msg)
    }

    /// The next frame to write.
    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.transmits.pop_front()
    }

    fn handle_event(&mut self, evt: Input) -> Result<(), Self::Error> {
        self.event(evt);
        Ok(())
    }

    fn poll_event(&mut self) -> Option<Self::Eout> {
        self.events.pop_front()
    }

    /// Tells the machine the time. The driver calls it before other input
    /// in each loop turn, and when the time from `poll_timeout` comes.
    fn handle_timeout(&mut self, now: Instant) -> Result<(), Self::Error> {
        self.timeout(now);
        Ok(())
    }

    /// The time at which the machine wants `handle_timeout`.
    fn poll_timeout(&mut self) -> Option<Self::Time> {
        self.next_wake()
    }

    /// Completes every request with `Closed`. A bound session logs out
    /// first, then the machine emits `Closed` and `CloseTransport`.
    fn close(&mut self) -> Result<(), Self::Error> {
        self.close_connection();
        Ok(())
    }
}
