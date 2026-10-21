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

//! The LED task. It obeys the newest command and ignores the older ones.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::GPIO15;

/// The shortest blink half-period. A smaller value would starve the other tasks.
const MIN_BLINK_MS: u32 = 10;

/// The newest command for the LED. The Iggy task signals it, the LED task waits on it.
pub static COMMAND: Signal<CriticalSectionRawMutex, LedCommand> = Signal::new();

/// What the LED does until the next command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedCommand {
    On,
    Off,
    /// Toggles the LED every `n` milliseconds.
    Blink(u32),
}

impl LedCommand {
    /// Reads `on`, `off` or `blink <milliseconds>`. Returns `None` for any other text.
    pub fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let command = match words.next()? {
            "on" => Self::On,
            "off" => Self::Off,
            "blink" => {
                let ms: u32 = words.next()?.parse().ok()?;
                Self::Blink(ms.max(MIN_BLINK_MS))
            }
            _ => return None,
        };
        // Extra words after a complete command make the text invalid.
        words.next().is_none().then_some(command)
    }
}

/// Drives the user LED on GPIO15. The LED is active low: low turns it on.
#[embassy_executor::task]
pub async fn task(pin: GPIO15<'static>) -> ! {
    let mut led = Output::new(pin, Level::High, OutputConfig::default());
    let mut command = LedCommand::Off;

    loop {
        match command {
            LedCommand::On => {
                led.set_low();
                command = COMMAND.wait().await;
            }
            LedCommand::Off => {
                led.set_high();
                command = COMMAND.wait().await;
            }
            LedCommand::Blink(ms) => {
                led.toggle();
                let half_period = Timer::after(Duration::from_millis(u64::from(ms)));
                if let Either::First(next) = select(COMMAND.wait(), half_period).await {
                    command = next;
                }
            }
        }
    }
}
