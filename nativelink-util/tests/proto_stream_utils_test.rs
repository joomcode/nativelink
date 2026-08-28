// Copyright 2024 The NativeLink Authors. All rights reserved.
//
// Licensed under the Functional Source License, Version 1.1, Apache 2.0 Future License (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    See LICENSE file for details
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use nativelink_error::Error;
use nativelink_macro::nativelink_test;
use nativelink_proto::google::bytestream::WriteRequest;
use nativelink_util::common::DigestInfo;
use nativelink_util::proto_stream_utils::{
    WriteRequestStreamWrapper, WriteState, WriteStateWrapper,
};
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use tokio_stream::wrappers::UnboundedReceiverStream;

const INSTANCE_NAME: &str = "test-instance";

// Regression test for TraceMachina/nativelink#745.
#[nativelink_test]
async fn ensure_no_errors_if_only_first_message_has_resource_name_set() -> Result<(), Error> {
    const RAW_DATA: &str = "thisdatafoo";
    const DIGEST: DigestInfo = DigestInfo::new([0u8; 32], RAW_DATA.len() as u64);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<WriteRequest, Error>>();

    let message1 = WriteRequest {
        resource_name: format!(
            "{INSTANCE_NAME}/uploads/some-uuid/blobs/{}/{}",
            DIGEST.packed_hash(),
            DIGEST.size_bytes()
        ),
        write_offset: 0,
        finish_write: false,
        data: Bytes::from_static(&RAW_DATA.as_bytes()[..4]),
    };
    let message2 = WriteRequest {
        resource_name: String::new(),
        write_offset: 4,
        finish_write: false,
        data: Bytes::from_static(&RAW_DATA.as_bytes()[4..8]),
    };
    let message3 = WriteRequest {
        resource_name: String::new(),
        write_offset: 8,
        finish_write: true,
        data: Bytes::from_static(&RAW_DATA.as_bytes()[8..]),
    };

    {
        tx.send(Ok(message1.clone())).unwrap();
        tx.send(Ok(message2.clone())).unwrap();
        tx.send(Ok(message3.clone())).unwrap();
        drop(tx); // Close the channel.
    }

    let local_state = Arc::new(Mutex::new(WriteState::new(
        INSTANCE_NAME.to_string(),
        WriteRequestStreamWrapper::from(UnboundedReceiverStream::new(rx)).await?,
    )));
    let mut write_state_wrapper = WriteStateWrapper::new(local_state.clone());

    {
        // Ensure we transported our data properly.
        assert_eq!(write_state_wrapper.next().await, Some(message1));
        assert_eq!(write_state_wrapper.next().await, Some(message2));
        assert_eq!(write_state_wrapper.next().await, Some(message3));
        assert_eq!(write_state_wrapper.next().await, None);

        // Ensure no stream errors were set.
        assert_eq!(local_state.lock().take_read_stream_error(), None);
    }

    Ok(())
}

/// Builds a `WriteState` over `count` 4-byte messages and drains `taken` of
/// them through the wrapper, so the replay buffer holds the last two taken.
async fn state_after_taking(
    count: usize,
    taken: usize,
) -> Result<
    Arc<Mutex<WriteState<UnboundedReceiverStream<Result<WriteRequest, Error>>, Error>>>,
    Error,
> {
    const CHUNK: &[u8] = b"abcd";
    let size = (count * CHUNK.len()) as u64;
    let digest = DigestInfo::new([0u8; 32], size);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<WriteRequest, Error>>();
    for i in 0..count {
        tx.send(Ok(WriteRequest {
            resource_name: if i == 0 {
                format!(
                    "{INSTANCE_NAME}/uploads/some-uuid/blobs/{}/{}",
                    digest.packed_hash(),
                    digest.size_bytes()
                )
            } else {
                String::new()
            },
            write_offset: (i * CHUNK.len()) as i64,
            finish_write: i + 1 == count,
            data: Bytes::from_static(CHUNK),
        }))
        .unwrap();
    }
    drop(tx);

    let local_state = Arc::new(Mutex::new(WriteState::new(
        INSTANCE_NAME.to_string(),
        WriteRequestStreamWrapper::from(UnboundedReceiverStream::new(rx)).await?,
    )));
    let mut wrapper = WriteStateWrapper::new(local_state.clone());
    for _ in 0..taken {
        assert!(wrapper.next().await.is_some());
    }
    Ok(local_state)
}

/// A resume is only safe while the replay buffer still holds the whole stream.
///
/// Past that point the replay starts at a non-zero `write_offset`, which only a
/// server that still holds the partial upload for this UUID accepts. A retry
/// re-resolves the endpoint and can reach a different backend, which has
/// received nothing and answers `Received out of order data`. That is
/// `InvalidArgument`, which is permanent and fails the whole write.
#[nativelink_test]
async fn resume_is_refused_once_the_replay_cannot_start_at_offset_zero() -> Result<(), Error> {
    // Nothing taken yet: the retry re-sends the first message, offset 0.
    assert!(
        state_after_taking(4, 0).await?.lock().can_resume(),
        "a write that sent nothing must resume"
    );

    // One and two messages taken are both still wholly inside the buffer.
    assert!(
        state_after_taking(4, 1).await?.lock().can_resume(),
        "the buffer still holds the whole stream after one message"
    );
    assert!(
        state_after_taking(4, 2).await?.lock().can_resume(),
        "the buffer still holds the whole stream after two messages"
    );

    // Three taken: the buffer holds messages at offsets 4 and 8, so the first
    // 4 bytes are gone and the replay would start at offset 4.
    assert!(
        !state_after_taking(4, 3).await?.lock().can_resume(),
        "a replay starting past offset 0 must be refused"
    );
    assert!(
        !state_after_taking(4, 4).await?.lock().can_resume(),
        "a replay starting past offset 0 must be refused"
    );

    Ok(())
}
