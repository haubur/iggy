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

//! The partition routes of one connection, ported from
//! `core/sdk/src/driver/route_cache.rs`.
//!
//! A route has no age limit. It goes stale after a refusal or a failure,
//! and a stale route keeps its view as the lowest view that its next lookup
//! accepts. So a lookup never goes back to an older primary of the same
//! partition history.

use alloc::collections::BTreeMap;
use bytes::Bytes;
use iggy_binary_protocol::WireEncode;
use iggy_binary_protocol::WireIdentifier;
use iggy_binary_protocol::requests::messages::GetPartitionRouteRequest;
use iggy_binary_protocol::responses::messages::PartitionRouteResponse;

/// A partition as its caller spelled it. A stream named by number and the
/// same stream named by name are two keys. The key is the body of the
/// route lookup, which is also its canonical spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PartitionKey {
    lookup: Bytes,
    partition_id: u32,
}

impl PartitionKey {
    pub fn new(stream_id: WireIdentifier, topic_id: WireIdentifier, partition_id: u32) -> Self {
        let request = GetPartitionRouteRequest {
            stream_id,
            topic_id,
            partition_id,
        };
        Self {
            lookup: request.to_bytes(),
            partition_id,
        }
    }

    /// The body of a `GET_PARTITION_ROUTE` request for this partition.
    pub fn lookup_body(&self) -> Bytes {
        self.lookup.clone()
    }

    pub const fn partition_id(&self) -> u32 {
        self.partition_id
    }
}

#[derive(Debug)]
struct Entry {
    route: PartitionRouteResponse,
    /// The session fence at the lookup. A route serves only that session.
    fence: u64,
    stale: bool,
    /// The lowest view that the next lookup accepts.
    floor: u32,
    /// Orders the entries for eviction.
    lookup: u64,
}

#[derive(Debug)]
pub struct RouteCache {
    entries: BTreeMap<PartitionKey, Entry>,
    lookups: u64,
    capacity: usize,
}

impl RouteCache {
    pub const fn new(capacity: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lookups: 0,
            capacity,
        }
    }

    /// The route of `key`, unless it is stale, from another session, or
    /// older than the metadata `watermark` that the session saw.
    pub fn usable(
        &self,
        key: &PartitionKey,
        fence: u64,
        watermark: u64,
    ) -> Option<PartitionRouteResponse> {
        self.entries
            .get(key)
            .filter(|entry| {
                !entry.stale && entry.fence == fence && entry.route.metadata_op >= watermark
            })
            .map(|entry| entry.route)
    }

    /// Takes a lookup answer for `key`, unless it is older than the cached
    /// route. Another history wins only with metadata at least as new.
    /// Within one history, a view below the floor loses, and so does the
    /// same view with another primary. The metadata facts of the answer
    /// never replace newer ones.
    pub fn merge(
        &mut self,
        key: &PartitionKey,
        answer: PartitionRouteResponse,
        fence: u64,
    ) -> Option<PartitionRouteResponse> {
        let mut route = answer;
        if let Some(cached) = self.entries.get(key) {
            let cached_route = cached.route;
            if route.incarnation == cached_route.incarnation {
                let above_floor = route.view >= cached.floor;
                let newer = above_floor
                    && (route.view > cached_route.view
                        || route.primary_replica == cached_route.primary_replica);
                if !newer {
                    return None;
                }
                if route.metadata_op < cached_route.metadata_op {
                    route.metadata_op = cached_route.metadata_op;
                    route.partitions_count = cached_route.partitions_count;
                }
            } else if route.metadata_op < cached_route.metadata_op {
                return None;
            }
        } else if self.entries.len() >= self.capacity {
            self.evict_oldest();
        }
        if self.capacity == 0 {
            return Some(route);
        }
        self.lookups += 1;
        self.entries.insert(
            key.clone(),
            Entry {
                route,
                fence,
                stale: false,
                floor: route.view,
                lookup: self.lookups,
            },
        );
        Some(route)
    }

    /// Marks the history of `route` stale after a refusal, a failure, or a
    /// change of the history.
    pub fn invalidate(&mut self, route: &PartitionRouteResponse, partition_id: u32) {
        self.mark_stale(route, partition_id, 0);
    }

    /// Marks the history of `route` stale after its primary answered that
    /// it is not the primary. The next lookup accepts no view up to this one.
    pub fn not_primary(&mut self, route: &PartitionRouteResponse, partition_id: u32) {
        self.mark_stale(route, partition_id, route.view.saturating_add(1));
    }

    /// Every spelling of one partition history shares its staleness.
    fn mark_stale(&mut self, route: &PartitionRouteResponse, partition_id: u32, floor: u32) {
        for (key, entry) in &mut self.entries {
            if key.partition_id == partition_id && entry.route.incarnation == route.incarnation {
                entry.stale = true;
                entry.floor = entry.floor.max(floor);
            }
        }
    }

    fn evict_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.lookup)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FENCE: u64 = 1;
    const METADATA_OP: u64 = 10;
    const INCARNATION: u64 = 7;
    const PARTITION_ID: u32 = 3;

    fn key(partition_id: u32) -> PartitionKey {
        PartitionKey::new(
            WireIdentifier::numeric(1),
            WireIdentifier::numeric(2),
            partition_id,
        )
    }

    fn answer(
        incarnation: u64,
        metadata_op: u64,
        view: u32,
        primary: u8,
    ) -> PartitionRouteResponse {
        PartitionRouteResponse {
            incarnation,
            metadata_op,
            view,
            partitions_count: 4,
            primary_replica: primary,
        }
    }

    #[test]
    fn older_answers_lose_and_newer_metadata_stays() {
        let mut cache = RouteCache::new(8);
        let partition = key(PARTITION_ID);
        let route = cache
            .merge(&partition, answer(INCARNATION, METADATA_OP, 5, 2), FENCE)
            .unwrap();
        for refused in [
            answer(INCARNATION, METADATA_OP, 4, 1),
            answer(INCARNATION, METADATA_OP, 5, 1),
            answer(INCARNATION - 1, METADATA_OP - 1, 9, 0),
        ] {
            assert_eq!(cache.merge(&partition, refused, FENCE), None);
        }
        assert_eq!(cache.usable(&partition, FENCE, METADATA_OP), Some(route));
        assert_eq!(cache.usable(&partition, FENCE + 1, METADATA_OP), None);
        assert_eq!(cache.usable(&partition, FENCE, METADATA_OP + 1), None);
    }

    #[test]
    fn not_primary_raises_the_floor_of_the_next_lookup() {
        let mut cache = RouteCache::new(8);
        let partition = key(PARTITION_ID);
        let route = cache
            .merge(&partition, answer(INCARNATION, METADATA_OP, 5, 2), FENCE)
            .unwrap();
        cache.not_primary(&route, PARTITION_ID);
        assert_eq!(cache.usable(&partition, FENCE, 0), None);
        assert_eq!(cache.merge(&partition, route, FENCE), None);
        assert!(
            cache
                .merge(&partition, answer(INCARNATION, METADATA_OP, 6, 1), FENCE)
                .is_some()
        );
    }

    #[test]
    fn a_full_cache_evicts_the_oldest_lookup() {
        let mut cache = RouteCache::new(1);
        cache.merge(&key(1), answer(1, 1, 1, 0), FENCE).unwrap();
        cache.merge(&key(2), answer(2, 1, 1, 0), FENCE).unwrap();
        assert_eq!(cache.usable(&key(1), FENCE, 0), None);
        assert!(cache.usable(&key(2), FENCE, 0).is_some());
    }
}
