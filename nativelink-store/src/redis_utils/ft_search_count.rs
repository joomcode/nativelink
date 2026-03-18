// Copyright 2024-2026 The NativeLink Authors. All rights reserved.
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

use nativelink_error::{Code, Error, ResultExt, make_err};
use redis::Value;
use redis::aio::ConnectionLike;

/// Returns the number of documents matching `query` on `RediSearch` index `index`, without loading
/// document payloads (`LIMIT 0 0`).
pub(crate) async fn ft_search_count<C: ConnectionLike + Send>(
    connection_manager: &mut C,
    index: &str,
    query: &str,
) -> Result<usize, Error> {
    let res: Value = redis::cmd("FT.SEARCH")
        .arg(index)
        .arg(query)
        .arg("LIMIT")
        .arg(0_i64)
        .arg(0_i64)
        .query_async(connection_manager)
        .await
        .err_tip(|| format!("FT.SEARCH count index={index} query={query:?}"))?;

    parse_ft_search_total(res).err_tip(|| format!("parse FT.SEARCH total index={index}"))
}

fn parse_ft_search_total(value: Value) -> Result<usize, Error> {
    match value {
        Value::Array(arr) if !arr.is_empty() => int_to_document_count(&arr[0]),
        Value::Map(ref entries) => {
            for (k, v) in entries.iter() {
                let Value::SimpleString(key) = k else {
                    continue;
                };
                if key == "total_results" {
                    return int_to_document_count(v);
                }
            }
            Err(make_err!(
                Code::Internal,
                "FT.SEARCH map missing total_results: {value:?}"
            ))
        }
        other => Err(make_err!(
            Code::Internal,
            "Unexpected FT.SEARCH response: {other:?}"
        )),
    }
}

fn int_to_document_count(v: &Value) -> Result<usize, Error> {
    let Value::Int(n) = v else {
        return Err(make_err!(
            Code::Internal,
            "FT.SEARCH count field not integer: {v:?}"
        ));
    };
    usize::try_from(*n)
        .map_err(|_| make_err!(Code::Internal, "Invalid document count from FT.SEARCH: {n}"))
}
