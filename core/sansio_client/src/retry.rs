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

//! The pause between two attempts of one request, ported from `backoff` in
//! `core/sdk/src/links/mod.rs`.

use core::time::Duration;

/// The ceiling of the first pause.
pub const RETRY_BACKOFF_BASE: Duration = Duration::from_millis(50);
/// The ceiling never grows beyond this.
pub const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The full jitter pause before retry `retry`: a random time in
/// `[0, min(max, base * 2^retry)]`. `draw` is the random fraction of that
/// range, as a numerator over `u32::MAX`.
pub fn backoff(retry: u32, draw: u32) -> Duration {
    let ceiling = 1_u32
        .checked_shl(retry)
        .and_then(|factor| RETRY_BACKOFF_BASE.checked_mul(factor))
        .map_or(RETRY_BACKOFF_MAX, |pause| pause.min(RETRY_BACKOFF_MAX));
    let micros = u64::try_from(ceiling.as_micros()).unwrap_or(u64::MAX);
    let scaled = u128::from(micros) * u128::from(draw) / u128::from(u32::MAX);
    Duration::from_micros(u64::try_from(scaled).unwrap_or(micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_doubles_up_to_the_cap() {
        assert_eq!(backoff(0, u32::MAX), Duration::from_millis(50));
        assert_eq!(backoff(1, u32::MAX), Duration::from_millis(100));
        assert_eq!(backoff(4, u32::MAX), Duration::from_millis(800));
        assert_eq!(backoff(5, u32::MAX), Duration::from_secs(1));
        assert_eq!(backoff(40, u32::MAX), Duration::from_secs(1));
        assert_eq!(backoff(3, 0), Duration::ZERO);
        assert_eq!(backoff(1, u32::MAX / 2), Duration::from_micros(49_999));
    }
}
