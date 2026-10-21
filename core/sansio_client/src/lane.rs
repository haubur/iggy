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

//! Lanes, ported from `core/sdk/src/session_owner/lanes.rs`.
//!
//! A lane admits one mutation at a time, in arrival order. There is one
//! lane for metadata and one for each partition history. A mutation whose
//! outcome is unknown stays retained in its lane, and the lane admits
//! nothing else until a resolve or an abandon frees it. So a retained
//! operation is always the newest operation of its lane, and its replay can
//! never age out.

use crate::error::ClientError;
use crate::header::Frame;
use crate::route::PartitionKey;
use alloc::collections::{BTreeMap, VecDeque};
use iggy_binary_protocol::PartitionContext;
use iggy_binary_protocol::consensus::{Operation, operation_for_code};

/// The id of a call inside the machine. Calls are numbered in arrival
/// order, so the lowest waiting id enters a free lane first.
pub type CallId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaneKey {
    Metadata,
    /// The incarnation names one history of the partition for every
    /// spelling of its stream and topic.
    Partition {
        incarnation: u64,
        partition_id: u32,
    },
}

impl LaneKey {
    /// The lane of `code`. A partition write takes the lane of the
    /// incarnation in its context. Reads, Register and Logout take no lane
    /// by code alone. Progress polls take their partition lane through
    /// [`Self::partition`].
    pub fn of(code: u32, context: &PartitionContext, partition_id: u32) -> Option<Self> {
        match operation_for_code(code) {
            Operation::NonReplicated | Operation::Register | Operation::Logout => None,
            operation if operation.is_partition() => Some(Self::partition(context, partition_id)),
            _ => Some(Self::Metadata),
        }
    }

    pub const fn partition(context: &PartitionContext, partition_id: u32) -> Self {
        Self::Partition {
            incarnation: context.incarnation,
            partition_id,
        }
    }
}

/// Whether a request of `code` changes state, so that its frame can be
/// retained.
pub fn is_mutation(code: u32) -> bool {
    !matches!(
        operation_for_code(code),
        Operation::NonReplicated | Operation::Register | Operation::Logout
    )
}

/// A frozen mutation. Every attempt and every replay sends the same frame.
#[derive(Debug, Clone)]
pub struct Retained {
    pub code: u32,
    pub frame: Frame,
    /// The partition of a partition write. A replay looks up no route,
    /// because the frame carries its context.
    pub partition: Option<PartitionKey>,
}

impl Retained {
    pub const fn op(&self) -> u64 {
        self.frame.header.request
    }
}

#[derive(Debug, Default)]
struct Lane {
    holder: Option<CallId>,
    retained: Option<Retained>,
}

/// Whether a call can go on in its lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Entered,
    Wait,
}

#[derive(Debug)]
pub struct Lanes {
    table: BTreeMap<LaneKey, Lane>,
    /// The outcomes that replays proved, oldest first. A session reset
    /// keeps them, because request ids never repeat within a client.
    proved: VecDeque<(u64, Result<(), ClientError>)>,
    proved_limit: usize,
}

impl Lanes {
    pub const fn new(proved_limit: usize) -> Self {
        Self {
            table: BTreeMap::new(),
            proved: VecDeque::new(),
            proved_limit,
        }
    }

    /// Lets `call` into the lane of `key`. A lane that retains an operation
    /// fails the call with `LaneBlocked`. A lane that another call holds
    /// makes it wait.
    pub fn enter(&mut self, key: LaneKey, call: CallId) -> Result<Entry, ClientError> {
        let lane = self.table.entry(key).or_default();
        if let Some(retained) = &lane.retained {
            return Err(ClientError::LaneBlocked(retained.op()));
        }
        match lane.holder {
            Some(holder) if holder != call => Ok(Entry::Wait),
            _ => {
                lane.holder = Some(call);
                Ok(Entry::Entered)
            }
        }
    }

    /// Takes the lane of the retained operation `op` for a replay. The lane
    /// keeps `op` retained, so new calls still fail with `LaneBlocked`.
    /// `None` if no lane retains `op`.
    pub fn resume(&mut self, op: u64, call: CallId) -> Option<(LaneKey, Entry, Retained)> {
        let (key, lane) = self
            .table
            .iter_mut()
            .find(|(_, lane)| lane.retained.as_ref().is_some_and(|r| r.op() == op))?;
        let retained = lane.retained.clone()?;
        let entry = match lane.holder {
            Some(holder) if holder != call => Entry::Wait,
            _ => {
                lane.holder = Some(call);
                Entry::Entered
            }
        };
        Some((*key, entry, retained))
    }

    /// The operation `op`, while a lane retains it.
    pub fn retained(&self, op: u64) -> Option<Retained> {
        self.table
            .values()
            .find_map(|lane| lane.retained.clone().filter(|r| r.op() == op))
    }

    /// Lets `call` leave its lane with a definitive outcome.
    pub fn leave(&mut self, key: LaneKey, call: CallId) {
        if let Some(lane) = self.table.get_mut(&key)
            && lane.holder == Some(call)
        {
            lane.holder = None;
            self.remove_if_idle(key);
        }
    }

    /// Lets `call` leave its lane with an unknown outcome. The lane retains
    /// `retained` until a resolve or an abandon.
    pub fn retain(&mut self, key: LaneKey, call: CallId, retained: Retained) {
        let lane = self.table.entry(key).or_default();
        if lane.holder == Some(call) {
            lane.holder = None;
        }
        lane.retained = Some(retained);
    }

    /// Ends the replay of `op` with a proved outcome and frees its lane.
    pub fn prove(&mut self, key: LaneKey, call: CallId, op: u64, outcome: Result<(), ClientError>) {
        if self.proved_limit > 0 {
            if self.proved.len() == self.proved_limit {
                self.proved.pop_front();
            }
            self.proved.push_back((op, outcome));
        }
        if let Some(lane) = self.table.get_mut(&key) {
            if lane.retained.as_ref().is_some_and(|r| r.op() == op) {
                lane.retained = None;
            }
            if lane.holder == Some(call) {
                lane.holder = None;
            }
            self.remove_if_idle(key);
        }
    }

    /// Frees the lane of the retained operation `op`. Its outcome stays
    /// unknown.
    pub fn abandon(&mut self, op: u64) -> Result<(), ClientError> {
        let key = self
            .table
            .iter_mut()
            .find(|(_, lane)| lane.retained.as_ref().is_some_and(|r| r.op() == op))
            .map(|(key, lane)| {
                lane.retained = None;
                *key
            })
            .ok_or(ClientError::OperationNotRetained(op))?;
        self.remove_if_idle(key);
        Ok(())
    }

    /// The outcome that a replay proved for `op`, while the machine keeps it.
    pub fn proved_outcome(&self, op: u64) -> Option<Result<(), ClientError>> {
        self.proved
            .iter()
            .find(|(proved, _)| *proved == op)
            .map(|(_, outcome)| outcome.clone())
    }

    /// After a session reset no retained operation can replay, and its
    /// outcome stays unknown.
    pub fn drop_retained(&mut self) {
        for lane in self.table.values_mut() {
            lane.retained = None;
        }
        self.table.retain(|_, lane| lane.holder.is_some());
    }

    /// Frees every lane, for a closed connection.
    pub fn clear(&mut self) {
        self.table.clear();
    }

    fn remove_if_idle(&mut self, key: LaneKey) {
        if self
            .table
            .get(&key)
            .is_some_and(|lane| lane.holder.is_none() && lane.retained.is_none())
        {
            self.table.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::CLIENT;
    use bytes::Bytes;
    use iggy_binary_protocol::RequestHeader;
    use iggy_binary_protocol::codes::CREATE_STREAM_CODE;

    fn retained(op: u64) -> Retained {
        Retained {
            code: CREATE_STREAM_CODE,
            frame: Frame {
                header: RequestHeader::for_request(CREATE_STREAM_CODE, CLIENT, op, 1, b"x")
                    .unwrap(),
                body: Bytes::from_static(b"x"),
            },
            partition: None,
        }
    }

    #[test]
    fn a_lane_admits_one_call_in_order() {
        let mut lanes = Lanes::new(4);
        assert_eq!(lanes.enter(LaneKey::Metadata, 1), Ok(Entry::Entered));
        assert_eq!(lanes.enter(LaneKey::Metadata, 2), Ok(Entry::Wait));
        lanes.leave(LaneKey::Metadata, 1);
        assert_eq!(lanes.enter(LaneKey::Metadata, 2), Ok(Entry::Entered));
    }

    #[test]
    fn a_retained_operation_blocks_its_lane_until_proved_or_abandoned() {
        let mut lanes = Lanes::new(4);
        lanes.enter(LaneKey::Metadata, 1).unwrap();
        lanes.retain(LaneKey::Metadata, 1, retained(7));
        assert_eq!(
            lanes.enter(LaneKey::Metadata, 2),
            Err(ClientError::LaneBlocked(7))
        );

        let (key, entry, _) = lanes.resume(7, 3).unwrap();
        assert_eq!(entry, Entry::Entered);
        assert_eq!(
            lanes.enter(LaneKey::Metadata, 2),
            Err(ClientError::LaneBlocked(7))
        );
        lanes.prove(key, 3, 7, Ok(()));
        assert_eq!(lanes.proved_outcome(7), Some(Ok(())));
        assert_eq!(lanes.enter(LaneKey::Metadata, 2), Ok(Entry::Entered));
        lanes.leave(LaneKey::Metadata, 2);

        lanes.enter(LaneKey::Metadata, 4).unwrap();
        lanes.retain(LaneKey::Metadata, 4, retained(8));
        lanes.abandon(8).unwrap();
        assert_eq!(lanes.abandon(8), Err(ClientError::OperationNotRetained(8)));
        assert_eq!(lanes.enter(LaneKey::Metadata, 5), Ok(Entry::Entered));
    }
}
