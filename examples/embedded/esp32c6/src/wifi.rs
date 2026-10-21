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

use embassy_net::Runner;
use embassy_time::{Duration, Timer};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config, Interface, WifiController};
use log::{info, warn};

/// Wi-Fi network name, read at build time.
pub const SSID: &str = env!("WIFI_SSID");
/// Wi-Fi password, read at build time.
pub const PASSWORD: &str = env!("WIFI_PASSWORD");
/// Address of the Iggy server as `ip:port`, read at build time.
pub const IGGY_ADDR: &str = env!("IGGY_ADDR");

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Keeps the station connected to the access point and reconnects after a drop.
#[embassy_executor::task]
pub async fn connection(mut controller: WifiController<'static>) {
    let config = Config::Station(
        StationConfig::default()
            .with_ssid(SSID.try_into().expect("WIFI_SSID is too long"))
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                PASSWORD.try_into().expect("WIFI_PASSWORD has a bad length"),
            )),
    );
    controller
        .set_config(&config)
        .expect("Failed to set the Wi-Fi configuration");

    loop {
        info!("Wi-Fi: connecting to \"{SSID}\"");
        match controller.connect_async().await {
            Ok(_) => {
                info!("Wi-Fi: connected");
                // Blocks until the access point drops the link.
                match controller.wait_for_disconnect_async().await {
                    Ok(info) => warn!("Wi-Fi: disconnected: {info:?}"),
                    Err(e) => warn!("Wi-Fi: disconnect wait failed: {e:?}"),
                }
            }
            Err(e) => warn!("Wi-Fi: connect failed: {e:?}"),
        }
        Timer::after(RECONNECT_DELAY).await;
    }
}

/// Drives the embassy-net stack: it moves packets between the stack and the Wi-Fi driver.
#[embassy_executor::task]
pub async fn net_runner(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}
