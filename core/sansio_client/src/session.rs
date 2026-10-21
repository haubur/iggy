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

//! The consensus session, ported from `core/sdk/src/session.rs` and the
//! fence of `core/sdk/src/session_owner.rs`.
//!
//! The client keeps one identity (client id and bind secret) for the life
//! of a logical session, across reconnects and login retries. The server
//! numbers the session at login. Requests count up from 1 and never repeat
//! within a client, so a reset keeps the counter.

use crate::entropy::{self, Entropy};
use crate::error::ClientError;
use alloc::boxed::Box;
use iggy_binary_protocol::requests::system::SessionIdentity;
use iggy_binary_protocol::requests::users::login_register::{BIND_SECRET_BYTES, BindSecret};

#[derive(Debug)]
pub struct Session {
    client_id: u128,
    bind_secret: BindSecret,
    /// The session number of the committed Register. `None` until bound.
    number: Option<u64>,
    /// The next application request id. Register always uses 0.
    request_counter: u64,
    /// Every reset raises the fence, so a frame or a route made under an
    /// older fence belongs to a replaced session.
    fence: u64,
    /// The highest metadata commit that a reply showed. A reset keeps it.
    metadata_watermark: u64,
}

impl Session {
    pub fn new(entropy: &mut impl Entropy) -> Self {
        let (client_id, bind_secret) = identity(entropy);
        Self {
            client_id,
            bind_secret,
            number: None,
            request_counter: 1,
            fence: 0,
            metadata_watermark: 0,
        }
    }

    pub const fn client_id(&self) -> u128 {
        self.client_id
    }

    pub fn bind_secret(&self) -> BindSecret {
        self.bind_secret.clone()
    }

    pub const fn session(&self) -> Option<u64> {
        self.number
    }

    pub const fn is_bound(&self) -> bool {
        self.number.is_some()
    }

    pub const fn fence(&self) -> u64 {
        self.fence
    }

    pub const fn metadata_watermark(&self) -> u64 {
        self.metadata_watermark
    }

    pub fn observe_metadata_commit(&mut self, commit: u64) {
        self.metadata_watermark = self.metadata_watermark.max(commit);
    }

    /// The identity that a bind presents. `None` before the first bind.
    pub fn identity(&self) -> Option<SessionIdentity> {
        Some(SessionIdentity {
            client_id: self.client_id,
            session: self.number?,
            metadata_watermark: self.metadata_watermark,
        })
    }

    /// Binds `session`, but only while the fence is `fence`. So the reply to
    /// a Register of a replaced session cannot bind the new session. A
    /// repeated bind of the same session is accepted.
    pub const fn bind_at(&mut self, fence: u64, session: u64) -> Result<(), ClientError> {
        if session == 0 || fence != self.fence {
            return Err(ClientError::InvalidSession);
        }
        if let Some(bound) = self.number
            && bound != session
        {
            return Err(ClientError::SessionMismatch {
                expected: bound,
                actual: session,
            });
        }
        self.number = Some(session);
        Ok(())
    }

    /// The next application request id. It never wraps.
    pub fn next_request_id(&mut self) -> Result<u64, ClientError> {
        if !self.is_bound() {
            return Err(ClientError::Unauthenticated);
        }
        let id = self.request_counter;
        self.request_counter = id.checked_add(1).ok_or(ClientError::RequestIdExhausted)?;
        Ok(id)
    }

    /// Replaces the identity and raises the fence. The request counter and
    /// the watermark stay. The server refuses another user on the client id
    /// of an earlier Register, so a new sign-in needs a new client id.
    pub fn reset(&mut self, entropy: &mut impl Entropy) {
        let (client_id, bind_secret) = identity(entropy);
        self.client_id = client_id;
        self.bind_secret = bind_secret;
        self.number = None;
        self.fence += 1;
    }
}

fn identity(entropy: &mut impl Entropy) -> (u128, BindSecret) {
    let client_id = entropy::client_id(entropy);
    let mut secret = Box::new([0; BIND_SECRET_BYTES]);
    entropy.fill(secret.as_mut_slice());
    (client_id, BindSecret::new(secret))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Counter;

    #[test]
    fn requests_count_from_one_after_bind_and_a_reset_keeps_the_counter() {
        let mut entropy = Counter::default();
        let mut session = Session::new(&mut entropy);
        assert_eq!(session.next_request_id(), Err(ClientError::Unauthenticated));
        session.bind_at(0, 9).unwrap();
        assert_eq!(session.next_request_id(), Ok(1));
        assert_eq!(session.next_request_id(), Ok(2));

        let client_id = session.client_id();
        session.reset(&mut entropy);
        assert_ne!(session.client_id(), client_id);
        assert_eq!(session.fence(), 1);
        assert!(!session.is_bound());
        assert_eq!(session.bind_at(0, 10), Err(ClientError::InvalidSession));
        session.bind_at(1, 10).unwrap();
        assert_eq!(session.next_request_id(), Ok(3));
    }

    #[test]
    fn a_bound_session_rejects_another_session() {
        let mut session = Session::new(&mut Counter::default());
        session.bind_at(0, 4).unwrap();
        session.bind_at(0, 4).unwrap();
        assert_eq!(
            session.bind_at(0, 5),
            Err(ClientError::SessionMismatch {
                expected: 4,
                actual: 5
            })
        );
        assert_eq!(session.bind_at(0, 0), Err(ClientError::InvalidSession));
    }
}
