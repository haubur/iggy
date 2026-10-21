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

//! A blocking driver for the sans-IO client over `std::net::TcpStream`.
//!
//! It signs in as the default user, makes a stream and a topic, sends 10
//! messages, polls them back, stores the offset, and stays idle for 30 s so
//! that the heartbeat runs. Start a server first:
//!
//! ```sh
//! cargo run --bin iggy-server
//! cargo run -p iggy_sansio --example blocking_tcp --features std
//! ```
//!
//! Set `IGGY_TCP_ADDRESS` to use another server than `127.0.0.1:8090`.

use bytes::Bytes;
use core::time::Duration;
use iggy_sansio::iggy_binary_protocol::codes::{CREATE_STREAM_CODE, CREATE_TOPIC_CODE};
use iggy_sansio::iggy_binary_protocol::primitives::options::WireOptions;
use iggy_sansio::iggy_binary_protocol::requests::consumer_offsets::StoreConsumerOffsetRequest;
use iggy_sansio::iggy_binary_protocol::requests::messages::PollMessagesRequest;
use iggy_sansio::iggy_binary_protocol::requests::streams::CreateStreamRequest;
use iggy_sansio::iggy_binary_protocol::requests::topics::CreateTopicRequest;
use iggy_sansio::iggy_binary_protocol::{
    AckLevel, WireConsumer, WireEncode, WireIdentifier, WireName, WirePollingStrategy,
};
use iggy_sansio::{
    ClientError, Command, Config, Connection, Entropy, Input, Instant, Message, Output, Protocol,
    Reply, RequestHandle, SecretString, Submit,
};
use std::collections::HashMap;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;

const STREAM: &str = "sansio-example";
const TOPIC: &str = "events";
const PARTITION_ID: u32 = 0;
const CONSUMER_ID: u32 = 1;
const MESSAGES: usize = 10;
const IDLE: Duration = Duration::from_secs(30);
/// The longest that one read blocks, so the loop sees new requests.
const READ_SLICE: Duration = Duration::from_millis(100);
const REDIAL_PAUSE: Duration = Duration::from_secs(1);
const STREAM_NAME_ALREADY_EXISTS: u32 = 1012;
const TOPIC_NAME_ALREADY_EXISTS: u32 = 2013;

/// The random source of the operating system.
struct OsEntropy;

impl Entropy for OsEntropy {
    fn fill(&mut self, buf: &mut [u8]) {
        rand::fill(buf);
    }
}

/// Moves bytes between one TCP socket and the machine, and dials again
/// when the machine asks to close the transport.
struct Driver {
    conn: Connection<OsEntropy>,
    address: String,
    socket: Option<TcpStream>,
    origin: std::time::Instant,
    next_handle: u64,
    finished: HashMap<RequestHandle, Result<Reply, ClientError>>,
    closed: bool,
}

impl Driver {
    fn new(address: String) -> Result<Self, ClientError> {
        Ok(Self {
            conn: Connection::new(Config::default(), OsEntropy)?,
            address,
            socket: None,
            origin: std::time::Instant::now(),
            next_handle: 0,
            finished: HashMap::new(),
            closed: false,
        })
    }

    fn now(&self) -> Instant {
        Instant::from_micros(u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    /// Sends one command and runs the loop until it completes.
    fn call(&mut self, command: Command) -> io::Result<Result<Reply, ClientError>> {
        let handle = RequestHandle(self.next_handle);
        self.next_handle += 1;
        self.conn
            .handle_timeout(self.now())
            .map_err(io::Error::other)?;
        if let Err(error) = self.conn.handle_write(Submit {
            handle,
            command,
            timeout: None,
        }) {
            return Ok(Err(error));
        }
        loop {
            self.turn()?;
            if let Some(result) = self.finished.remove(&handle) {
                return Ok(result);
            }
        }
    }

    /// Runs the loop for `duration`.
    fn idle(&mut self, duration: Duration) -> io::Result<()> {
        let until = std::time::Instant::now() + duration;
        while std::time::Instant::now() < until {
            self.turn()?;
        }
        Ok(())
    }

    /// One turn of the driver loop.
    fn turn(&mut self) -> io::Result<()> {
        if self.socket.is_none() && !self.closed {
            self.dial();
        }
        self.conn
            .handle_timeout(self.now())
            .map_err(io::Error::other)?;
        self.flush();
        self.drain();
        let now = self.now();
        let wait = self
            .conn
            .poll_timeout()
            .map_or(READ_SLICE, |at| at.saturating_duration_since(now))
            .clamp(Duration::from_millis(1), READ_SLICE);
        let Some(socket) = &mut self.socket else {
            return Ok(());
        };
        socket.set_read_timeout(Some(wait))?;
        let mut buffer = [0; 16 * 1024];
        match socket.read(&mut buffer) {
            Ok(0) => self.lost("the server closed the connection"),
            Ok(read) => {
                self.conn
                    .handle_timeout(self.now())
                    .map_err(io::Error::other)?;
                self.conn
                    .handle_read(Bytes::copy_from_slice(&buffer[..read]))
                    .map_err(io::Error::other)?;
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => self.lost(&error.to_string()),
        }
        self.flush();
        self.drain();
        Ok(())
    }

    fn dial(&mut self) {
        match TcpStream::connect(&self.address) {
            Ok(socket) => {
                let _ = socket.set_nodelay(true);
                println!("Connected to {}.", self.address);
                self.socket = Some(socket);
                let _ = self.conn.handle_timeout(self.now());
                let _ = self.conn.handle_event(Input::Connected);
            }
            Err(error) => {
                println!("Failed to connect to {}: {error}", self.address);
                std::thread::sleep(REDIAL_PAUSE);
            }
        }
    }

    /// Writes every frame that the machine queued, header then body.
    fn flush(&mut self) {
        while let Some(transmit) = self.conn.poll_write() {
            let Some(socket) = &mut self.socket else {
                // The machine resends what it needs after the next connect.
                continue;
            };
            let written = socket
                .write_all(&transmit.header)
                .and_then(|()| socket.write_all(&transmit.body));
            if let Err(error) = written {
                self.lost(&error.to_string());
            }
        }
    }

    fn drain(&mut self) {
        while let Some(completion) = self.conn.poll_read() {
            self.finished.insert(completion.handle, completion.result);
        }
        while let Some(event) = self.conn.poll_event() {
            match event {
                Output::SignedIn {
                    user_id, session, ..
                } => {
                    println!("Signed in as user {user_id}, session {session}.");
                }
                Output::Resumed { session } => println!("Resumed session {session}."),
                Output::SessionReset => println!("The server lost the session. Sign in again."),
                Output::Evicted { reason, .. } => println!("Evicted: {reason:?}."),
                Output::CloseTransport => {
                    println!("The machine closed the connection.");
                    self.socket = None;
                }
                Output::Closed => {
                    self.closed = true;
                    self.socket = None;
                }
            }
        }
    }

    fn lost(&mut self, reason: &str) {
        println!("Lost the connection: {reason}");
        self.socket = None;
        let _ = self.conn.handle_event(Input::Disconnected);
    }
}

fn main() -> Result<(), Box<dyn core::error::Error>> {
    let address =
        std::env::var("IGGY_TCP_ADDRESS").unwrap_or_else(|_| String::from("127.0.0.1:8090"));
    let mut driver = Driver::new(address)?;

    driver.call(Command::LoginUser {
        username: String::from("iggy"),
        password: SecretString::from("iggy"),
    })??;

    let stream = CreateStreamRequest {
        name: WireName::new(STREAM)?,
        options: WireOptions::empty(),
    };
    allow_existing(
        driver.call(raw(CREATE_STREAM_CODE, stream.to_bytes()))?,
        STREAM_NAME_ALREADY_EXISTS,
    )?;
    let topic = CreateTopicRequest {
        stream_id: WireIdentifier::named(STREAM)?,
        partitions_count: 1,
        name: WireName::new(TOPIC)?,
        options: WireOptions::empty(),
    };
    allow_existing(
        driver.call(raw(CREATE_TOPIC_CODE, topic.to_bytes()))?,
        TOPIC_NAME_ALREADY_EXISTS,
    )?;

    let messages = (0..MESSAGES)
        .map(|index| Message::new(Bytes::from(format!("message {index}"))))
        .collect();
    driver.call(Command::SendMessages {
        stream_id: WireIdentifier::named(STREAM)?,
        topic_id: WireIdentifier::named(TOPIC)?,
        partition_id: PARTITION_ID,
        messages,
    })??;
    println!("Sent {MESSAGES} messages.");

    let consumer = WireConsumer::consumer(WireIdentifier::numeric(CONSUMER_ID));
    let polled = driver.call(Command::PollMessages(PollMessagesRequest {
        consumer: consumer.clone(),
        stream_id: WireIdentifier::named(STREAM)?,
        topic_id: WireIdentifier::named(TOPIC)?,
        partition_id: Some(PARTITION_ID),
        strategy: WirePollingStrategy::first(),
        count: u32::try_from(MESSAGES)?,
        auto_commit: false,
    }))??;
    let Reply::Polled(polled) = polled else {
        return Err("a poll returned another reply".into());
    };
    let mut last_offset = None;
    for message in polled.messages() {
        let message = message?;
        println!(
            "Polled offset {}: {}",
            message.offset,
            String::from_utf8_lossy(message.payload)
        );
        last_offset = Some(message.offset);
    }

    if let Some(offset) = last_offset {
        driver.call(Command::StoreConsumerOffset(StoreConsumerOffsetRequest {
            consumer,
            stream_id: WireIdentifier::named(STREAM)?,
            topic_id: WireIdentifier::named(TOPIC)?,
            partition_id: Some(PARTITION_ID),
            offset,
            ack: AckLevel::default(),
        }))??;
        println!("Stored offset {offset}.");
    }

    println!(
        "Idle for {} s. The heartbeat keeps the session.",
        IDLE.as_secs()
    );
    driver.idle(IDLE)?;
    driver.call(Command::Ping)??;
    println!("Ping answered.");

    driver.conn.close()?;
    while !driver.closed {
        driver.turn()?;
    }
    println!("Logged out and closed.");
    Ok(())
}

const fn raw(code: u32, payload: Bytes) -> Command {
    Command::Raw { code, payload }
}

/// Accepts a refusal with `exists`, so that the example can run twice.
fn allow_existing(result: Result<Reply, ClientError>, exists: u32) -> Result<(), ClientError> {
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.code() == Some(exists) => Ok(()),
        Err(error) => Err(error),
    }
}
