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

//! An Embassy driver for the sans-IO Iggy client.
//!
//! One task owns the TCP socket and the `iggy_sansio::Connection`. It signs in, sends a
//! temperature reading to `esp32/telemetry`, polls `esp32/commands` and hands each command to
//! the LED task. The state machine never touches the socket or the clock. This driver moves
//! the bytes and tells the machine the time, as `blocking_tcp.rs` does on a laptop.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bytes::Bytes;
use core::fmt::Write as _;
use core::future::pending;
use core::net::SocketAddrV4;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant as EmbassyInstant, Timer};
use embedded_io_async::Write;
use iggy_sansio::iggy_binary_protocol::requests::messages::PollMessagesRequest;
use iggy_sansio::iggy_binary_protocol::{WireConsumer, WireIdentifier, WirePollingStrategy};
use iggy_sansio::{
    ClientError, Command, Completion, Config, Connection, Entropy, Input, Instant, Message, Output,
    Protocol, Reply, RequestHandle, SecretString, Submit, codes,
};
use log::{info, warn};

use crate::led::{self, LedCommand};
use crate::sensor::{READINGS, Reading};

const USERNAME: &str = env!("IGGY_USERNAME");
const PASSWORD: &str = env!("IGGY_PASSWORD");

const STREAM: &str = "esp32";
const TELEMETRY_TOPIC: &str = "telemetry";
const COMMANDS_TOPIC: &str = "commands";
const PARTITION_ID: u32 = 0;
/// The consumer id that the board uses for its offset on the `commands` topic.
const CONSUMER_ID: u32 = 1;

const RX_BUFFER_SIZE: usize = 4096;
const TX_BUFFER_SIZE: usize = 4096;
const READ_CHUNK: usize = 2048;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_EVERY: Duration = Duration::from_millis(500);
const POLL_COUNT: u32 = 10;
const RECONNECT_PAUSE: Duration = Duration::from_secs(2);
/// The pause after the server refused the credentials. It stops the board from hammering.
const BAD_CREDENTIALS_PAUSE: Duration = Duration::from_secs(30);

// The low two bits of a request handle tell the driver what a completion belongs to. The bits
// above them count the requests, so each handle is unique among the open requests.
const KIND_MASK: u64 = 0b11;
const KIND_LOGIN: u64 = 0;
const KIND_SEND: u64 = 1;
const KIND_POLL: u64 = 2;
const KIND_RESOLVE: u64 = 3;

/// The random source of the board: the true random number generator of the chip.
///
/// The bind secret proves that this client owns its session, so it must be truly random. The
/// caller keeps the `TrngSource` alive for the whole program.
pub struct EspEntropy(pub esp_hal::rng::Trng);

impl Entropy for EspEntropy {
    fn fill(&mut self, buf: &mut [u8]) {
        self.0.read(buf);
    }
}

fn now() -> Instant {
    Instant::from_micros(EmbassyInstant::now().as_micros())
}

/// Why one connected session ended. A larger value wins when two reasons meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum End {
    /// The link or the session is gone. Dial again after a short pause.
    Lost,
    /// The server refused the user name or the password. Wait before the next try.
    BadCredentials,
}

/// The work that can wake the serve loop besides the socket and the machine timer.
enum Work {
    Reading(Reading),
    PollTick,
}

struct Driver {
    conn: Connection<EspEntropy>,
    stream: WireIdentifier,
    telemetry_topic: WireIdentifier,
    commands_topic: WireIdentifier,
    request_count: u64,
    /// True from `SignedIn` or `Resumed` until the next connect.
    ready: bool,
    /// These flags stay set across a reconnect. The machine retries a lost request after the
    /// next connect, and the request ends at its deadline at the latest.
    login_pending: bool,
    poll_pending: bool,
    /// The `seq` of each send that is open.
    sends: BTreeMap<RequestHandle, u32>,
    /// The operation of each resolve that is open.
    resolves: BTreeMap<RequestHandle, u64>,
    /// Writes with an unknown outcome. Each one blocks its partition until a resolve ends it.
    uncertain: Vec<u64>,
}

impl Driver {
    fn new(entropy: EspEntropy) -> Result<Self, ClientError> {
        let config = Config {
            request_timeout: core::time::Duration::from_secs(10), // a dead link shows in 10 s
            max_in_flight: 4,
            max_queued: 8,
            max_frame_size: 16 * 1024, // the default is 64 MiB
            max_cached_routes: 4,
            max_proved_outcomes: 4,
            ..Config::default() // keeps the 5 s heartbeat
        };
        Ok(Self {
            conn: Connection::new(config, entropy)?,
            stream: WireIdentifier::named(STREAM)?,
            telemetry_topic: WireIdentifier::named(TELEMETRY_TOPIC)?,
            commands_topic: WireIdentifier::named(COMMANDS_TOPIC)?,
            request_count: 0,
            ready: false,
            login_pending: false,
            poll_pending: false,
            sends: BTreeMap::new(),
            resolves: BTreeMap::new(),
            uncertain: Vec::new(),
        })
    }

    fn next_handle(&mut self, kind: u64) -> RequestHandle {
        self.request_count += 1;
        RequestHandle((self.request_count << 2) | kind)
    }

    /// Hands a request to the machine. Returns the handle if the machine took it.
    fn submit(&mut self, kind: u64, command: Command) -> Option<RequestHandle> {
        let handle = self.next_handle(kind);
        let submit = Submit {
            handle,
            command,
            timeout: None,
        };
        match self.conn.handle_write(submit) {
            Ok(()) => Some(handle),
            Err(error) => {
                warn!("the machine refused a request of kind {kind}: {error}");
                None
            }
        }
    }

    fn submit_login(&mut self) {
        let command = Command::LoginUser {
            username: String::from(USERNAME),
            password: SecretString::from(PASSWORD),
        };
        self.login_pending = self.submit(KIND_LOGIN, command).is_some();
    }

    fn submit_send(&mut self, reading: Reading) {
        let mut json = String::new();
        if write!(
            json,
            "{{\"seq\":{},\"temp_c\":{:.1}}}",
            reading.seq, reading.temp_c
        )
        .is_err()
        {
            return;
        }
        // The id stays 0, so the server picks one. The origin time stays 0, because the board
        // has no wall clock.
        let command = Command::SendMessages {
            stream_id: self.stream.clone(),
            topic_id: self.telemetry_topic.clone(),
            partition_id: PARTITION_ID,
            messages: vec![Message::new(Bytes::from(json))],
        };
        if let Some(handle) = self.submit(KIND_SEND, command) {
            self.sends.insert(handle, reading.seq);
        }
    }

    fn submit_poll(&mut self) {
        // With `auto_commit` the server stores the offset. After a restart the board does not
        // replay old commands.
        let command = Command::PollMessages(PollMessagesRequest {
            consumer: WireConsumer::consumer(WireIdentifier::numeric(CONSUMER_ID)),
            stream_id: self.stream.clone(),
            topic_id: self.commands_topic.clone(),
            partition_id: Some(PARTITION_ID),
            strategy: WirePollingStrategy::next(),
            count: POLL_COUNT,
            auto_commit: true,
        });
        self.poll_pending = self.submit(KIND_POLL, command).is_some();
    }

    /// Replays each write with an unknown outcome. Without this the partition lane stays
    /// blocked, and every later request to that partition fails with `LaneBlocked`.
    fn submit_resolves(&mut self) {
        for op in core::mem::take(&mut self.uncertain) {
            if let Some(handle) = self.submit(KIND_RESOLVE, Command::Resolve(op)) {
                self.resolves.insert(handle, op);
            }
        }
    }

    fn remember_uncertain(&mut self, op: u64) {
        if !self.uncertain.contains(&op) {
            self.uncertain.push(op);
        }
    }

    /// Takes the session events of the machine.
    fn handle_events(&mut self) -> Option<End> {
        let mut end = None;
        while let Some(event) = self.conn.poll_event() {
            match event {
                Output::SignedIn {
                    user_id, session, ..
                } => {
                    info!("SignedIn as user {user_id}, session {session}");
                    self.ready = true;
                }
                Output::Resumed { session } => {
                    info!("Resumed session {session}");
                    self.ready = true;
                }
                Output::SessionReset => {
                    // The server lost the session. The next connect signs in again.
                    warn!("the server lost the session");
                    self.ready = false;
                    end = end.max(Some(End::Lost));
                }
                Output::Evicted { reason, .. } => {
                    warn!("evicted: {reason:?}");
                    self.ready = false;
                }
                Output::CloseTransport => {
                    info!("the machine closed the connection");
                    end = end.max(Some(End::Lost));
                }
                Output::Closed => end = end.max(Some(End::Lost)),
            }
        }
        end
    }

    /// Takes the finished requests of the machine and matches each one by its handle.
    fn handle_completions(&mut self) -> Option<End> {
        let mut end = None;
        while let Some(Completion { handle, result }) = self.conn.poll_read() {
            match (handle.0 & KIND_MASK, result) {
                (KIND_LOGIN, result) => {
                    self.login_pending = false;
                    if let Err(error) = result {
                        warn!("login failed: {error}");
                        let reason = if error.code() == Some(codes::INVALID_CREDENTIALS) {
                            End::BadCredentials
                        } else {
                            End::Lost
                        };
                        end = end.max(Some(reason));
                    }
                }
                (KIND_SEND, result) => {
                    let seq = self.sends.remove(&handle);
                    match result {
                        Ok(_) => match seq {
                            Some(seq) => info!("sent seq={seq}"),
                            None => info!("sent a retried reading"),
                        },
                        Err(ClientError::OperationUncertain(op)) => {
                            warn!("send seq={seq:?} has an unknown outcome, resolving op {op}");
                            self.remember_uncertain(op);
                        }
                        Err(error) => warn!("send seq={seq:?} failed: {error}"),
                    }
                }
                (KIND_POLL, result) => {
                    self.poll_pending = false;
                    match result {
                        Ok(Reply::Polled(polled)) => {
                            for message in polled.messages() {
                                match message {
                                    Ok(message) => apply_command(message.offset, message.payload),
                                    Err(error) => {
                                        warn!("a polled message does not decode: {error}");
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(_) => warn!("a poll returned another reply"),
                        Err(error) => warn!("poll failed: {error}"),
                    }
                }
                (KIND_RESOLVE, result) => {
                    let op = self.resolves.remove(&handle);
                    match result {
                        Ok(_) => info!("resolved op {op:?}"),
                        Err(ClientError::OperationUncertain(op)) => self.remember_uncertain(op),
                        // The link went away before the answer. Try the same op again.
                        Err(ClientError::Disconnected | ClientError::Timeout) => {
                            if let Some(op) = op {
                                self.remember_uncertain(op);
                            }
                        }
                        Err(error) => warn!("resolve of op {op:?} failed: {error}"),
                    }
                }
                _ => {}
            }
        }
        end
    }

    /// Writes every frame that the machine queued, header then body.
    async fn write_frames(&mut self, socket: &mut TcpSocket<'_>) -> Result<(), ()> {
        while let Some(transmit) = self.conn.poll_write() {
            if let Err(error) = socket.write_all(&transmit.header).await {
                warn!("write failed: {error:?}");
                return Err(());
            }
            if let Err(error) = socket.write_all(&transmit.body).await {
                warn!("write failed: {error:?}");
                return Err(());
            }
        }
        Ok(())
    }

    /// Serves one connected socket until it is lost or the machine asks to close it.
    async fn serve(&mut self, socket: &mut TcpSocket<'_>, buf: &mut [u8]) -> End {
        let mut next_poll = EmbassyInstant::now();
        loop {
            // The clock first, so that deadlines and the heartbeat fire.
            if let Err(error) = self.conn.handle_timeout(now()) {
                warn!("timeout handling failed: {error}");
                return End::Lost;
            }
            // Do not log in twice: a bound session binds again by itself after a reconnect.
            if !self.ready && !self.conn.is_signed_in() && !self.login_pending {
                self.submit_login();
            }
            // Other requests wait for `SignedIn` or `Resumed`. Before it they fail.
            if self.ready {
                self.submit_resolves();
            }
            if self.write_frames(socket).await.is_err() {
                return End::Lost;
            }
            let end = self.handle_events().max(self.handle_completions());
            if let Some(end) = end {
                return end;
            }

            let ready = self.ready;
            let wake = self
                .conn
                .poll_timeout()
                .unwrap_or_else(|| now() + core::time::Duration::from_secs(1));
            let poll_at = next_poll;
            let work = async {
                if ready {
                    match select(READINGS.receive(), Timer::at(poll_at)).await {
                        Either::First(reading) => Work::Reading(reading),
                        Either::Second(()) => Work::PollTick,
                    }
                } else {
                    pending().await
                }
            };
            let woke = select3(
                socket.read(&mut buf[..READ_CHUNK]),
                Timer::at(EmbassyInstant::from_micros(wake.as_micros())),
                work,
            )
            .await;

            match woke {
                Either3::First(Ok(0)) => {
                    info!("the server closed the connection");
                    return End::Lost;
                }
                Either3::First(Ok(read)) => {
                    let handled = self
                        .conn
                        .handle_timeout(now())
                        .and_then(|()| self.conn.handle_read(Bytes::copy_from_slice(&buf[..read])));
                    if let Err(error) = handled {
                        warn!("the machine rejected the bytes: {error}");
                        return End::Lost;
                    }
                }
                Either3::First(Err(error)) => {
                    warn!("read failed: {error:?}");
                    return End::Lost;
                }
                Either3::Second(()) => {}
                Either3::Third(Work::Reading(reading)) => self.submit_send(reading),
                Either3::Third(Work::PollTick) => {
                    next_poll = EmbassyInstant::now() + POLL_EVERY;
                    // Skip the tick while the last poll is still open.
                    if !self.poll_pending {
                        self.submit_poll();
                    }
                }
            }
        }
    }

    /// Dials the server and serves the socket until it is lost. Returns the pause before the
    /// next try.
    async fn session(
        &mut self,
        stack: Stack<'static>,
        remote: SocketAddrV4,
        rx: &mut [u8],
        tx: &mut [u8],
        buf: &mut [u8],
    ) -> Duration {
        let mut socket = TcpSocket::new(stack, rx, tx);
        socket.set_timeout(Some(SOCKET_TIMEOUT));
        info!("connecting to {remote}");
        if let Err(error) = socket.connect(remote).await {
            warn!("connect to {remote} failed: {error:?}");
            return RECONNECT_PAUSE;
        }
        info!("connected to {remote}");

        self.ready = false;
        let _ = self.conn.handle_timeout(now());
        let _ = self.conn.handle_event(Input::Connected);

        let end = self.serve(&mut socket, buf).await;

        // Tell the machine that the transport is gone. Then drop the frames that it queued for
        // the old socket. It resends what it needs after the next connect.
        let _ = self.conn.handle_timeout(now());
        let _ = self.conn.handle_event(Input::Disconnected);
        while self.conn.poll_write().is_some() {}
        self.handle_events();
        self.handle_completions();

        socket.abort();
        let _ = socket.flush().await;
        match end {
            End::Lost => RECONNECT_PAUSE,
            End::BadCredentials => BAD_CREDENTIALS_PAUSE,
        }
    }
}

/// Reads one polled message and signals the LED task. Any text but a command is logged and
/// skipped.
fn apply_command(offset: u64, payload: &[u8]) {
    let Ok(text) = core::str::from_utf8(payload) else {
        warn!("skipped the message at offset {offset}: it is not UTF-8");
        return;
    };
    match LedCommand::parse(text) {
        Some(command) => {
            info!("command at offset {offset}: {text:?}");
            led::COMMAND.signal(command);
        }
        None => warn!("skipped the message at offset {offset}: {text:?} is not a command"),
    }
}

/// Keeps one Iggy session alive, sends telemetry and obeys commands.
#[embassy_executor::task]
pub async fn task(stack: Stack<'static>, remote: SocketAddrV4, entropy: EspEntropy) -> ! {
    let mut driver = Driver::new(entropy).expect("The client configuration is valid");
    let mut rx = [0u8; RX_BUFFER_SIZE];
    let mut tx = [0u8; TX_BUFFER_SIZE];
    let mut buf = [0u8; READ_CHUNK];

    loop {
        stack.wait_config_up().await;
        let pause = driver
            .session(stack, remote, &mut rx, &mut tx, &mut buf)
            .await;
        Timer::after(pause).await;
    }
}
