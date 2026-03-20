# Plan: PropertyRouterScheduler

Routes incoming actions to different backend schedulers based on a
platform property value (e.g. `container-image`), so the client always
talks to one endpoint and knows nothing about the internal topology.

## Architecture

```
Bazel Client
    │
    │ ExecuteRequest
    ▼
Front NativeLink Process
    ├── ExecutionServer
    │       │
    │       │ add_action(action_info)
    │       ▼
    │   PropertyRouterScheduler
    │       │
    │       │ reads action_info.platform_properties["container-image"]
    │       │
    │       ├── "compile" / "test-env" / "test-fat-env"
    │       │       └── GrpcScheduler ──► Scheduler Process 1
    │       │                                   │
    │       │                               Workers (compile, test)
    │       │
    │       └── anything else (default)
    │               └── GrpcScheduler ──► Scheduler Process 2
    │                                           │
    │                                       Workers (default)
    │
    └── worker_api (not exposed on front process — managed by backend processes)
```

## Files Changed: 8 total (3 new, 5 modified)

### New files

| File | Description |
|------|-------------|
| `nativelink-scheduler/src/property_router_scheduler.rs` | Core implementation |
| `nativelink-scheduler/tests/property_router_scheduler_test.rs` | Unit tests |
| `docs/property-router-scheduler-plan.md` | This file |

### Modified files

| File | Change |
|------|--------|
| `nativelink-config/src/schedulers.rs` | Add `PropertyRouterSpec` struct and `SchedulerSpec::PropertyRouter` variant |
| `nativelink-scheduler/src/lib.rs` | Register `property_router_scheduler` module |
| `nativelink-scheduler/src/default_scheduler_factory.rs` | Add match arm for `PropertyRouter` |

---

## Step 1 — Config

**File:** `nativelink-config/src/schedulers.rs`

Add after `PropertyModifierSpec`:

```rust
/// Routes actions to different schedulers based on a platform property value.
/// Actions whose property value matches a key in `routes` go to that scheduler.
/// All other actions (missing property or unmatched value) go to `default_scheduler`.
#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "dev-schema", derive(JsonSchema))]
pub struct PropertyRouterSpec {
    /// The platform property key to match on (e.g. "container-image").
    #[serde(deserialize_with = "convert_string_with_shellexpand")]
    pub property_name: String,

    /// Map of property value -> nested scheduler spec.
    pub routes: HashMap<String, SchedulerSpec>,

    /// Scheduler to use when the property is absent or its value does not match any route.
    pub default_scheduler: Box<SchedulerSpec>,
}
```

Add variant to `SchedulerSpec`:

```rust
pub enum SchedulerSpec {
    Simple(SimpleSpec),
    Grpc(GrpcSpec),
    CacheLookup(CacheLookupSpec),
    PropertyModifier(PropertyModifierSpec),
    PropertyRouter(PropertyRouterSpec),   // <-- new
}
```

---

## Step 2 — Core Implementation

**File:** `nativelink-scheduler/src/property_router_scheduler.rs`

Follows the exact same pattern as `property_modifier_scheduler.rs`.

### Struct

```rust
#[derive(MetricsComponent)]
pub struct PropertyRouterScheduler {
    property_name: String,
    #[metric(group = "routes")]
    routes: HashMap<String, Arc<dyn ClientStateManager>>,
    #[metric(group = "default_scheduler")]
    default_scheduler: Arc<dyn ClientStateManager>,
}

impl PropertyRouterScheduler {
    pub fn new(
        property_name: &str,
        routes: HashMap<String, Arc<dyn ClientStateManager>>,
        default_scheduler: Arc<dyn ClientStateManager>,
    ) -> Self {
        Self {
            property_name: property_name.to_string(),
            routes,
            default_scheduler,
        }
    }
}
```

### `add_action` — the core routing logic

Reads the property value from `action_info.platform_properties`
(`HashMap<String, String>`), looks it up in `routes`, falls back to
`default_scheduler`:

```rust
async fn inner_add_action(
    &self,
    client_operation_id: OperationId,
    action_info: Arc<ActionInfo>,
) -> Result<Box<dyn ActionStateResult>, Error> {
    let scheduler = action_info
        .platform_properties
        .get(&self.property_name)
        .and_then(|value| self.routes.get(value))
        .unwrap_or(&self.default_scheduler);

    scheduler.add_action(client_operation_id, action_info).await
}
```

### `filter_operations` — fan-out to all schedulers

The caller (e.g. `WaitExecution`) does not know which backend scheduler
holds the operation, so the router must query all of them and merge:

```rust
async fn inner_filter_operations(
    &self,
    filter: OperationFilter,
) -> Result<ActionStateResultStream<'_>, Error> {
    let mut streams = Vec::with_capacity(self.routes.len() + 1);
    for scheduler in self.routes.values() {
        streams.push(scheduler.filter_operations(filter.clone()).await?);
    }
    streams.push(self.default_scheduler.filter_operations(filter).await?);
    Ok(Box::pin(futures::stream::select_all(streams)))
}
```

`OperationFilter` is already `Clone` (derives it at line 67 of
`nativelink-util/src/operation_state_manager.rs`).

### `KnownPlatformPropertyProvider` — union of all nested schedulers

```rust
async fn inner_get_known_properties(
    &self,
    instance_name: &str,
) -> Result<Vec<String>, Error> {
    let mut all_props = HashSet::new();
    for scheduler in self.routes.values() {
        if let Some(p) = scheduler.as_known_platform_property_provider() {
            for prop in p.get_known_properties(instance_name).await? {
                all_props.insert(prop);
            }
        }
    }
    if let Some(p) = self.default_scheduler.as_known_platform_property_provider() {
        for prop in p.get_known_properties(instance_name).await? {
            all_props.insert(prop);
        }
    }
    Ok(all_props.into_iter().collect())
}
```

### Trait impls

Implements `ClientStateManager`, `KnownPlatformPropertyProvider`,
`RootMetricsComponent`. Does **not** implement `WorkerScheduler` — the
router never manages workers directly.

---

## Step 3 — Register Module

**File:** `nativelink-scheduler/src/lib.rs`

Add:

```rust
pub mod property_router_scheduler;
```

---

## Step 4 — Factory

**File:** `nativelink-scheduler/src/default_scheduler_factory.rs`

Add import at the top:

```rust
use crate::property_router_scheduler::PropertyRouterScheduler;
```

Add match arm in `inner_scheduler_factory` after `PropertyModifier`:

```rust
SchedulerSpec::PropertyRouter(spec) => {
    let mut routes = HashMap::with_capacity(spec.routes.len());
    for (value, nested_spec) in &spec.routes {
        let (action_scheduler, _) = Box::pin(inner_scheduler_factory(
            nested_spec,
            store_manager,
            maybe_origin_event_tx,
        ))
        .await
        .err_tip(|| format!("In nested PropertyRouterScheduler route '{value}'"))?;
        routes.insert(
            value.clone(),
            action_scheduler.err_tip(|| {
                format!("Nested route '{value}' is not an action scheduler")
            })?,
        );
    }
    let (default_action_scheduler, _) = Box::pin(inner_scheduler_factory(
        &spec.default_scheduler,
        store_manager,
        maybe_origin_event_tx,
    ))
    .await
    .err_tip(|| "In PropertyRouterScheduler default_scheduler")?;
    let router = Arc::new(PropertyRouterScheduler::new(
        &spec.property_name,
        routes,
        default_action_scheduler
            .err_tip(|| "Default scheduler is not an action scheduler")?,
    ));
    (Some(router), None)
}
```

---

## Step 5 — Tests

**File:** `nativelink-scheduler/tests/property_router_scheduler_test.rs`

Uses `MockActionScheduler` — same pattern as `property_modifier_scheduler_test.rs`.

### Test fixture

```rust
struct TestContext {
    compile_scheduler: Arc<MockActionScheduler>,
    default_scheduler: Arc<MockActionScheduler>,
    router: PropertyRouterScheduler,
}

fn make_router() -> TestContext {
    let compile_scheduler = Arc::new(MockActionScheduler::new());
    let default_scheduler = Arc::new(MockActionScheduler::new());
    let mut routes = HashMap::new();
    routes.insert(
        "compile".to_string(),
        compile_scheduler.clone() as Arc<dyn ClientStateManager>,
    );
    let router = PropertyRouterScheduler::new(
        "container-image",
        routes,
        default_scheduler.clone() as Arc<dyn ClientStateManager>,
    );
    TestContext { compile_scheduler, default_scheduler, router }
}
```

### Tests

| # | Name | Scenario | Expected |
|---|------|----------|----------|
| 1 | `routes_to_matching_scheduler` | `container-image=compile` | `compile_scheduler.expect_add_action` fires, `default_scheduler` idle |
| 2 | `routes_to_default_when_no_match` | `container-image=other` | `default_scheduler.expect_add_action` fires, `compile_scheduler` idle |
| 3 | `routes_to_default_when_property_missing` | No `container-image` key | `default_scheduler.expect_add_action` fires |
| 4 | `routes_multiple_values` | Two actions: `compile` then `other` | Each routed to correct scheduler |
| 5 | `filter_operations_fans_out_to_all` | `filter_operations` called | Both `compile_scheduler` and `default_scheduler` receive the same filter |
| 6 | `known_properties_unions_all_schedulers` | `get_known_properties` called | Returns union of props from both schedulers |
| 7 | `error_from_nested_scheduler_propagates` | `compile_scheduler` returns `Err` | Router propagates the error |

### Example test (test #1)

```rust
#[nativelink_test]
async fn routes_to_matching_scheduler() -> Result<(), Error> {
    let ctx = make_router();
    let mut action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest())
        .as_ref()
        .clone();
    action_info
        .platform_properties
        .insert("container-image".to_string(), "compile".to_string());
    let action_info = Arc::new(action_info);

    let (_tx, rx) = watch::channel(Arc::new(ActionState {
        client_operation_id: OperationId::default(),
        stage: ActionStage::Queued,
        action_digest: action_info.unique_qualifier.digest(),
        last_transition_timestamp: SystemTime::now(),
    }));
    let client_operation_id = OperationId::default();

    let (_, (received_op_id, received_action)) = join!(
        ctx.router.add_action(client_operation_id.clone(), action_info.clone()),
        ctx.compile_scheduler.expect_add_action(Ok(Box::new(
            TokioWatchActionStateResult::new(client_operation_id.clone(), action_info, rx)
        ))),
    );
    assert_eq!(client_operation_id, received_op_id);
    assert_eq!(
        Some(&"compile".to_string()),
        received_action.platform_properties.get("container-image")
    );
    Ok(())
}
```

---

## Example Production Config

```json5
// scheduler.json5 (front process — one endpoint for all clients)
{
  stores: [
    {
      name: "CAS_STORE",
      grpc: {
        instance_name: "main",
        endpoints: [{ address: "grpc://cas-node:50051" }],
        store_type: "cas",
      },
    },
  ],
  schedulers: [
    {
      name: "MAIN_SCHEDULER",
      property_router: {
        property_name: "container-image",
        routes: {
          "compile":      { grpc: { endpoint: { address: "grpc://sched-compile:50052" } } },
          "test-env":     { grpc: { endpoint: { address: "grpc://sched-compile:50052" } } },
          "test-fat-env": { grpc: { endpoint: { address: "grpc://sched-compile:50052" } } },
        },
        default_scheduler: { grpc: { endpoint: { address: "grpc://sched-default:50052" } } },
      },
    },
  ],
  servers: [
    {
      listener: { http: { socket_address: "0.0.0.0:50052" } },
      services: {
        execution: [
          { instance_name: "",     cas_store: "CAS_STORE", scheduler: "MAIN_SCHEDULER" },
          { instance_name: "main", cas_store: "CAS_STORE", scheduler: "MAIN_SCHEDULER" },
        ],
        capabilities: [
          { instance_name: "",     remote_execution: { scheduler: "MAIN_SCHEDULER" } },
          { instance_name: "main", remote_execution: { scheduler: "MAIN_SCHEDULER" } },
        ],
        health: {},
      },
    },
  ],
}
```

---

## Notes

- `WorkerScheduler` is **not** implemented by the router — worker management
  stays entirely in the backend scheduler processes.
- The router does not cache the routing decision. This is intentional:
  `add_action` reads a `HashMap<String, String>` lookup — O(1), zero cost.
- `filter_operations` fan-out is necessary because `WaitExecution` uses it
  and does not know which backend scheduler owns the operation.
  With N backend schedulers this is N parallel gRPC calls — acceptable since
  it's used for status polling, not hot-path action dispatch.
