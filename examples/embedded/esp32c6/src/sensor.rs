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

//! The sensor task. It reads the on-chip temperature and hands the reading to the Iggy task.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Ticker};
use esp_hal::peripherals::TSENS;
use esp_hal::tsens::{Config, TemperatureSensor};
use log::warn;

const READ_EVERY: Duration = Duration::from_secs(2);

/// Readings that wait for the Iggy task. The sensor never blocks on this channel.
pub static READINGS: Channel<CriticalSectionRawMutex, Reading, 4> = Channel::new();

/// One temperature reading.
#[derive(Debug, Clone, Copy)]
pub struct Reading {
    /// Counts the readings since power-up. A gap in the numbers shows a dropped reading.
    pub seq: u32,
    pub temp_c: f32,
}

/// Reads the temperature every 2 seconds. When the channel is full, the board is offline or
/// slow, and the reading is dropped.
#[embassy_executor::task]
pub async fn task(tsens: TSENS<'static>) -> ! {
    let sensor = TemperatureSensor::new(tsens, Config::default())
        .expect("Failed to start the temperature sensor");
    let mut ticker = Ticker::every(READ_EVERY);
    let mut seq: u32 = 0;

    loop {
        ticker.next().await;
        let reading = Reading {
            seq,
            temp_c: sensor.get_temperature().to_celsius(),
        };
        seq = seq.wrapping_add(1);
        if READINGS.try_send(reading).is_err() {
            warn!("dropped reading seq={}: the channel is full", reading.seq);
        }
    }
}
