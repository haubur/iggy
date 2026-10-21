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

//! The clock of the machine. The driver reads the time, the machine only
//! compares and adds.

use core::ops::{Add, Sub};
use core::time::Duration;

/// A point in time, in microseconds from any monotonic origin that the
/// driver picks. Only differences between two instants mean anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(pub u64);

impl Instant {
    /// The instant `micros` microseconds after the origin.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// The microseconds since the origin.
    #[must_use]
    pub const fn as_micros(self) -> u64 {
        self.0
    }

    /// The time from `earlier` to `self`, zero if `earlier` is later.
    #[must_use]
    pub const fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_micros(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Self;

    /// Saturates at the end of time, so a long timeout never wraps.
    fn add(self, duration: Duration) -> Self {
        let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        Self(self.0.saturating_add(micros))
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;

    fn sub(self, duration: Duration) -> Self {
        let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        Self(self.0.saturating_sub(micros))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_a_huge_duration_saturates() {
        assert_eq!(Instant(5) + Duration::MAX, Instant(u64::MAX));
        assert_eq!(Instant(5) - Duration::from_secs(1), Instant(0));
        assert_eq!(
            Instant(3_000).saturating_duration_since(Instant(1_000)),
            Duration::from_millis(2)
        );
    }
}
