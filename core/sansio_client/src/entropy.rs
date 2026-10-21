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

//! The random source of the machine.

/// Random bytes from the driver.
///
/// The machine draws a client id and a bind secret for every new session
/// identity, and a fraction for every backoff pause. The bind secret proves
/// ownership of a session on a new connection, so the source must be
/// cryptographically secure: a hardware RNG, `getrandom`, or a CSPRNG seeded
/// from one.
pub trait Entropy {
    /// Fills `buf` with random bytes.
    fn fill(&mut self, buf: &mut [u8]);
}

impl<E: Entropy + ?Sized> Entropy for &mut E {
    fn fill(&mut self, buf: &mut [u8]) {
        (**self).fill(buf);
    }
}

/// A non-zero random client id. The server refuses a zero client id.
pub fn client_id(entropy: &mut impl Entropy) -> u128 {
    loop {
        let mut bytes = [0; size_of::<u128>()];
        entropy.fill(&mut bytes);
        let id = u128::from_le_bytes(bytes);
        if id != 0 {
            return id;
        }
    }
}

/// A random fraction in `[0, 1]`, as a numerator over `u32::MAX`.
pub fn fraction(entropy: &mut impl Entropy) -> u32 {
    let mut bytes = [0; size_of::<u32>()];
    entropy.fill(&mut bytes);
    u32::from_le_bytes(bytes)
}
