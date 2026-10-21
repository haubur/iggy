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

//! Request headers and reply decoding, ported from `core/sdk/src/vsr.rs`.
//!
//! Received headers are read field by field at their wire offsets: the
//! receive buffer is not 16-byte aligned, so a cast of a header with `u128`
//! fields can fail.

use crate::error::ClientError;
use crate::session::Session;
use bytes::Bytes;
use core::mem::offset_of;
use iggy_binary_protocol::codes::{LOGIN_REGISTER_CODE, LOGIN_REGISTER_WITH_PAT_CODE};
use iggy_binary_protocol::consensus::{
    Command, EvictionHeader, EvictionReason, GenericHeader, HEADER_SIZE, Operation, ReplyHeader,
    RequestHeader, operation_for_code, read_size_field, result_code, result_section_len,
};

/// The id of every request without a session. A bound id is never 0, and
/// the machine sends one unbound request at a time, so no reply to an
/// unbound request can answer another request.
pub const UNBOUND_REQUEST_ID: u64 = 0;

/// A request header and its body.
#[derive(Debug, Clone)]
pub struct Frame {
    pub header: RequestHeader,
    pub body: Bytes,
}

impl Frame {
    /// The header as the bytes that go on the wire.
    #[must_use]
    pub fn header_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0; HEADER_SIZE];
        bytes.copy_from_slice(bytemuck::bytes_of(&self.header));
        bytes
    }

    /// Whether the frame goes without a session, so its id is 0.
    #[must_use]
    pub const fn is_unbound(&self) -> bool {
        self.header.session == 0
    }
}

/// Builds the header of `code` with the identity of `session`.
///
/// A sign-in uses session 0 and request 0. A read uses session 0 and
/// request 0 before bind, and a new id after bind, so a reply names exactly
/// one request. A replicated request needs a bound session.
pub fn encode_request_header(
    session: &mut Session,
    code: u32,
    payload: &[u8],
) -> Result<RequestHeader, ClientError> {
    let (request_id, session_id) = match code {
        LOGIN_REGISTER_CODE | LOGIN_REGISTER_WITH_PAT_CODE => (UNBOUND_REQUEST_ID, 0),
        _ if operation_for_code(code) == Operation::NonReplicated => match session.session() {
            Some(session_id) => (session.next_request_id()?, session_id),
            None => (UNBOUND_REQUEST_ID, 0),
        },
        _ => {
            let session_id = session.session().ok_or(ClientError::Unauthenticated)?;
            (session.next_request_id()?, session_id)
        }
    };
    Ok(RequestHeader::for_request(
        code,
        session.client_id(),
        request_id,
        session_id,
        payload,
    )?)
}

/// The size of a frame from its first bytes, or `None` if it is not a
/// valid frame size.
pub fn frame_size(header: &[u8]) -> Option<usize> {
    let size = read_size_field(header)? as usize;
    (size >= HEADER_SIZE).then_some(size)
}

/// The frame type. `Command` sits at one offset in every header.
pub const fn peek_command(header: &[u8; HEADER_SIZE]) -> Command {
    const COMMAND_OFFSET: usize = offset_of!(GenericHeader, command);
    match header[COMMAND_OFFSET] {
        x if x == Command::Reply as u8 => Command::Reply,
        x if x == Command::Eviction as u8 => Command::Eviction,
        _ => Command::Reserved,
    }
}

/// The request that a reply answers.
pub fn reply_request_id(header: &[u8; HEADER_SIZE]) -> u64 {
    read_u64(header, offset_of!(ReplyHeader, request))
}

/// The metadata commit that a metadata reply shows, `None` for any other
/// frame. The session keeps the highest one as its watermark.
pub fn metadata_commit(header: &[u8; HEADER_SIZE]) -> Option<u64> {
    if peek_command(header) != Command::Reply {
        return None;
    }
    let operation = read_operation(header).ok()?;
    if operation == Operation::NonReplicated || operation.is_partition() {
        return None;
    }
    Some(read_u64(header, offset_of!(ReplyHeader, commit)))
}

/// The eviction that an `Eviction` frame carries. An unknown reason reads
/// as `NoSession`, so the session is tested again on the next bind.
pub fn decode_eviction(header: &[u8; HEADER_SIZE]) -> ClientError {
    ClientError::Evicted {
        reason: eviction_reason(header).unwrap_or(EvictionReason::NoSession),
        server_protocol_version: read_u32(
            header,
            offset_of!(EvictionHeader, server_protocol_version),
        ),
        server_protocol_version_min: read_u32(
            header,
            offset_of!(EvictionHeader, server_protocol_version_min),
        ),
    }
}

fn eviction_reason(header: &[u8; HEADER_SIZE]) -> Option<EvictionReason> {
    const REASON_OFFSET: usize = offset_of!(EvictionHeader, reason);
    bytemuck::checked::try_from_bytes::<EvictionReason>(&header[REASON_OFFSET..=REASON_OFFSET])
        .ok()
        .copied()
}

/// A decoded reply: the body or the refusal, and whether the reply proves
/// that the request committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyOutcome {
    pub result: Result<Bytes, ClientError>,
    pub committed: bool,
}

/// Decodes the reply to `request`. An `Err` means that the frame is
/// invalid, and the connection can no longer be trusted.
///
/// A nonzero status is a refusal before commit, with an empty body. A
/// result-framed reply carries success or a refusal in its result section.
/// The reply proves a commit only if it echoes the request exactly.
pub fn decode_reply(
    request: &RequestHeader,
    header: &[u8; HEADER_SIZE],
    body: Bytes,
) -> Result<ReplyOutcome, ClientError> {
    if peek_command(header) != Command::Reply {
        return Err(ClientError::InvalidReply);
    }
    let expected_body = frame_size(header).ok_or(ClientError::InvalidReply)? - HEADER_SIZE;
    if body.len() != expected_body || reply_request_id(header) != request.request {
        return Err(ClientError::InvalidReply);
    }
    let status = read_u32(header, offset_of!(ReplyHeader, status));
    if status != 0 {
        return Ok(ReplyOutcome {
            result: Err(ClientError::from_server_code(status)),
            committed: false,
        });
    }
    let reply = bytemuck::checked::try_pod_read_unaligned::<ReplyHeader>(header)
        .map_err(|_| ClientError::InvalidReply)?;
    let result = decode_result(reply.operation, body)?;
    let committed = reply.has_committed_result() && {
        let matches_request = reply.client == request.client
            && reply.request_checksum == request.request_checksum
            && (reply.operation == request.operation
                || matches!(
                    (request.operation, reply.operation),
                    // The primary resolves these commands before it commits them.
                    (
                        Operation::CreateTopic,
                        Operation::CreateTopicWithAssignments
                    ) | (
                        Operation::CreatePartitions,
                        Operation::CreatePartitionsWithAssignments
                    ) | (Operation::DeleteSegments, Operation::TruncatePartition)
                ))
            && reply.partition_incarnation == request.partition_incarnation
            && reply.op != 0
            && reply.op == reply.commit;
        if !matches_request {
            return Err(ClientError::InvalidReply);
        }
        !matches!(
            result,
            Err(ClientError::TransientNotCommitted
                | ClientError::TransientNotAccepted
                | ClientError::NotPrimary)
        )
    };
    Ok(ReplyOutcome { result, committed })
}

/// Strips the result section of a result-framed reply. A Register reply
/// with an empty body is a terminal failure without a section, and it
/// passes through to fail the typed decode.
fn decode_result(
    operation: Operation,
    body: Bytes,
) -> Result<Result<Bytes, ClientError>, ClientError> {
    let result_framed =
        operation.is_result_framed() || (operation == Operation::Register && !body.is_empty());
    if !result_framed {
        return Ok(Ok(body));
    }
    match result_code(&body) {
        Some(0) => {
            let start = result_section_len(&body).ok_or(ClientError::InvalidReply)?;
            Ok(Ok(body.slice(start..)))
        }
        Some(code) => Ok(Err(ClientError::from_server_code(code))),
        None => Err(ClientError::InvalidReply),
    }
}

fn read_operation(header: &[u8; HEADER_SIZE]) -> Result<Operation, ClientError> {
    const OPERATION_OFFSET: usize = offset_of!(ReplyHeader, operation);
    bytemuck::checked::try_from_bytes::<Operation>(&header[OPERATION_OFFSET..=OPERATION_OFFSET])
        .copied()
        .map_err(|_| ClientError::InvalidReply)
}

fn read_u32(header: &[u8; HEADER_SIZE], offset: usize) -> u32 {
    let mut bytes = [0; size_of::<u32>()];
    bytes.copy_from_slice(&header[offset..offset + size_of::<u32>()]);
    u32::from_le_bytes(bytes)
}

fn read_u64(header: &[u8; HEADER_SIZE], offset: usize) -> u64 {
    let mut bytes = [0; size_of::<u64>()];
    bytes.copy_from_slice(&header[offset..offset + size_of::<u64>()]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{CLIENT, refusal, reply, reply_header};
    use iggy_binary_protocol::codes::{CREATE_STREAM_CODE, PING_CODE};

    fn ping(request: u64) -> RequestHeader {
        RequestHeader::for_request(PING_CODE, CLIENT, request, 1, &[]).unwrap()
    }

    fn split(frame: &[u8]) -> ([u8; HEADER_SIZE], Bytes) {
        let mut header = [0; HEADER_SIZE];
        header.copy_from_slice(&frame[..HEADER_SIZE]);
        (header, Bytes::copy_from_slice(&frame[HEADER_SIZE..]))
    }

    #[test]
    fn a_reply_returns_its_body() {
        let (header, body) = split(&reply(5, b"pong"));
        let outcome = decode_reply(&ping(5), &header, body).unwrap();
        assert_eq!(outcome.result, Ok(Bytes::from_static(b"pong")));
        assert!(!outcome.committed);
    }

    #[test]
    fn a_reply_to_another_request_is_invalid() {
        let (header, body) = split(&reply(6, b""));
        assert_eq!(
            decode_reply(&ping(5), &header, body),
            Err(ClientError::InvalidReply)
        );
    }

    #[test]
    fn a_status_refuses_the_request() {
        let (header, body) = split(&refusal(5, 58));
        let outcome = decode_reply(&ping(5), &header, body).unwrap();
        assert_eq!(outcome.result, Err(ClientError::TransientNotAccepted));
    }

    #[test]
    fn a_result_section_carries_success_or_a_rejection() {
        let request = RequestHeader::for_request(CREATE_STREAM_CODE, CLIENT, 5, 1, b"x").unwrap();
        let mut success = 0_u32.to_le_bytes().to_vec();
        success.extend_from_slice(b"stream");
        let frame = reply_header(5, Operation::CreateStream, &success);
        let (header, body) = split(&frame);
        let outcome = decode_reply(&request, &header, body).unwrap();
        assert_eq!(outcome.result, Ok(Bytes::from_static(b"stream")));

        let mut rejection = 1_u32.to_le_bytes().to_vec();
        rejection.extend_from_slice(&0_u32.to_le_bytes());
        rejection.extend_from_slice(&57_u32.to_le_bytes());
        let frame = reply_header(5, Operation::CreateStream, &rejection);
        let (header, body) = split(&frame);
        let outcome = decode_reply(&request, &header, body).unwrap();
        assert_eq!(outcome.result, Err(ClientError::TransientNotCommitted));

        let frame = reply_header(5, Operation::CreateStream, &[1, 0]);
        let (header, body) = split(&frame);
        assert_eq!(
            decode_reply(&request, &header, body),
            Err(ClientError::InvalidReply)
        );
    }

    #[test]
    fn an_eviction_names_its_reason() {
        let header = crate::testing::eviction(EvictionReason::InvalidCredentials);
        assert_eq!(peek_command(&header), Command::Eviction);
        assert!(matches!(
            decode_eviction(&header),
            ClientError::Evicted {
                reason: EvictionReason::InvalidCredentials,
                ..
            }
        ));
    }

    #[test]
    fn a_metadata_reply_shows_its_commit() {
        let frame = reply_header(5, Operation::CreateStream, &0_u32.to_le_bytes());
        let (header, _) = split(&frame);
        assert_eq!(metadata_commit(&header), Some(crate::testing::COMMIT));
        let (header, _) = split(&reply(5, b""));
        assert_eq!(metadata_commit(&header), None);
    }
}
