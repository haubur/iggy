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

use iggy::prelude::*;
use integration::iggy_harness;

const DIGITS_ONLY_NAME: &str = "424242";

// todo(haubur): Without fix, fails for HTTP transport.
// All three integration tests should be green after fix is implemented.

// Illustrates, how an Identifier from a digit only stream name does not get
// the stream back.
// Currently, the HTTP API does not handle IdKinds, but infers the kind from the string.
// Everything that is digit only is cast to IdKind::Numeric, searching stream id's instead of names.
#[iggy_harness(test_client_transport = [Tcp, Http])]
async fn given_digit_only_stream_name_when_getting_the_stream_should_resolve_it_by_name(
    harness: &TestHarness,
) {
    let client = harness.root_client().await.unwrap();

    let created = client.create_stream(DIGITS_ONLY_NAME).await.unwrap();
    let stream_id = Identifier::named(DIGITS_ONLY_NAME).unwrap();

    // Should get the correct stream by casting the name into an Identifier.
    // If it fails, the server casts the the digit only name into an Identifier with IdKind::Numeric
    // and looks for that id rather the name.
    let fetched = client
        .get_stream(&stream_id)
        .await
        .unwrap()
        .unwrap_or_else(|| {
            panic!(
                "stream named {DIGITS_ONLY_NAME} must resolve by name, \
                 not as id {DIGITS_ONLY_NAME}"
            )
        });

    assert_eq!(fetched.name, DIGITS_ONLY_NAME);
    assert_eq!(fetched.id, created.id);
}

// Illustrates, how a stream with a name that is equal to another stream id does not get the
// stream with that (digit) name, but returns the stream with that id instead.
#[iggy_harness(test_client_transport = [Tcp, Http])]
async fn given_stream_name_equal_to_another_stream_id_when_getting_the_stream_should_resolve_it_by_name(
    harness: &TestHarness,
) {
    let client = harness.root_client().await.unwrap();
    let trap = client.create_stream("its-a-trap").await.unwrap();

    let wrong_hit_name = trap.id.to_string();
    let wrong_hit = client.create_stream(&wrong_hit_name).await.unwrap();
    assert_ne!(wrong_hit.id, trap.id);

    let stream_id = Identifier::named(&wrong_hit_name).unwrap();
    let fetched = client.get_stream(&stream_id).await.unwrap().unwrap();

    // Without fix we get the first stream back, not the second as intended.
    assert_eq!(
        fetched.id, wrong_hit.id,
        "a string identifier {wrong_hit_name} must resolve the stream of that \
         name, not the stream whose id is {}",
        trap.id
    );
    assert_eq!(fetched.name, wrong_hit_name);
}

// Illustrates the same behaviour from the first test but for topics, instead of streams.
#[iggy_harness(test_client_transport = [Tcp, Http])]
async fn given_digit_only_topic_name_when_getting_the_topic_should_resolve_it_by_name(
    harness: &TestHarness,
) {
    let client = harness.root_client().await.unwrap();
    let stream = client
        .create_stream("streams-with-digit-topics")
        .await
        .unwrap();
    let stream_id = Identifier::numeric(stream.id).unwrap();

    let created = client
        .create_topic(
            &stream_id,
            DIGITS_ONLY_NAME,
            &TopicCreateOptions {
                partitions_count: Some(1),
                message_expiry: Some(IggyExpiry::NeverExpire),
                ..TopicCreateOptions::default()
            },
        )
        .await
        .unwrap();

    let topic_id = Identifier::named(DIGITS_ONLY_NAME).unwrap();
    let fetched = client
        .get_topic(&stream_id, &topic_id)
        .await
        .unwrap()
        .unwrap_or_else(|| {
            panic!(
                "topic named {DIGITS_ONLY_NAME} must resolve by name, \
                 not as id {DIGITS_ONLY_NAME}"
            )
        });

    assert_eq!(fetched.name, DIGITS_ONLY_NAME);
    assert_eq!(fetched.id, created.id);
}
