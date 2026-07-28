// Copyright 2026 The NativeLink Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The identity allowlist is configured once per process, so these assertions
//! live in their own test binary and in a single test: they need to observe the
//! unconfigured default before installing an allowlist, which a second test
//! running in parallel would race.

use std::collections::HashSet;

use nativelink_util::metrics::{
    EXECUTION_IDENTITY_OTHER, EXECUTION_IDENTITY_UNKNOWN, identity_label, set_identity_allowlist,
};

#[test]
fn identity_label_reports_only_allowlisted_identities() {
    // Unconfigured: identities are not distinguished, so nothing a client sends
    // can create a label value.
    assert_eq!(identity_label("ci"), EXECUTION_IDENTITY_OTHER);
    assert_eq!(identity_label(""), EXECUTION_IDENTITY_UNKNOWN);

    // Empty entries are ignored -- an absent identity is already "unknown".
    set_identity_allowlist(["ci".to_string(), "local".to_string(), String::new()]).unwrap();

    assert_eq!(identity_label("ci"), "ci");
    assert_eq!(identity_label("local"), "local");
    assert_eq!(identity_label(""), EXECUTION_IDENTITY_UNKNOWN);

    // Allowlisted labels are handed out as the same `&'static str` every time,
    // so recording a metric never allocates for the attribute value.
    assert!(core::ptr::eq(identity_label("ci"), identity_label("ci")));

    // An identity that isn't allowlisted is bucketed, so a client sending a
    // fresh value per request cannot grow the label set.
    assert_eq!(identity_label("dev@example.com"), EXECUTION_IDENTITY_OTHER);
    let labels: HashSet<&str> = (0..1_000)
        .map(|i| identity_label(&format!("attacker-{i}")))
        .collect();
    assert_eq!(labels, HashSet::from([EXECUTION_IDENTITY_OTHER]));

    // Configuring twice is an error rather than a silent no-op.
    assert!(set_identity_allowlist(["late".to_string()]).is_err());
    assert_eq!(identity_label("late"), EXECUTION_IDENTITY_OTHER);
}
