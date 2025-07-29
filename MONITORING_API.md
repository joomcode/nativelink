# NativeLink Monitoring API

The NativeLink Monitoring API provides real-time information about the scheduler state, including connected workers, running operations, and system metrics.

## Configuration

To enable the monitoring service, add it to your NativeLink configuration:

```json5
{
  "servers": [
    {
      "name": "monitoring",
      "listener": {
        "http": {
          "socket_address": "0.0.0.0:50062"
        }
      },
      "services": {
        "monitoring": {
          "scheduler": "MAIN_SCHEDULER"
        }
      }
    }
  ]
}
```

## API Endpoints

### Workers

#### GET /api/v1/workers
Returns a list of all connected workers with their status and running operations.

**Response:**
```json
[
  {
    "id": "worker-123",
    "platform_properties": {
      "cpu_count": "8",
      "memory_kb": "16384",
      "cpu_arch": "x86_64",
      "OSFamily": "Linux"
    },
    "last_update_timestamp": 1640995200,
    "is_paused": false,
    "is_draining": false,
    "can_accept_work": true,
    "running_operations": ["op-456", "op-789"],
    "connected_timestamp": 1640995000,
    "actions_completed": 42
  }
]
```

#### GET /api/v1/workers/{worker_id}
Returns detailed information about a specific worker.

**Response:**
```json
{
  "id": "worker-123",
  "platform_properties": {
    "cpu_count": "8",
    "memory_kb": "16384",
    "cpu_arch": "x86_64",
    "OSFamily": "Linux"
  },
  "last_update_timestamp": 1640995200,
  "is_paused": false,
  "is_draining": false,
  "can_accept_work": true,
  "running_operations": ["op-456", "op-789"],
  "connected_timestamp": 1640995000,
  "actions_completed": 42
}
```

### Operations

#### GET /api/v1/operations
Returns a list of operations with optional filtering.

**Query Parameters:**
- `stage`: Filter by operation stage (`queued`, `executing`, `completed`, `cache_check`)
- `worker_id`: Filter by worker ID
- `limit`: Maximum number of operations to return (default: 1000)

**Response:**
```json
[
  {
    "operation_id": "op-456",
    "client_operation_id": "client-op-456",
    "stage": "Executing",
    "worker_id": "worker-123",
    "action_digest": "sha256:abc123...",
    "command_digest": "sha256:def456...",
    "input_root_digest": "sha256:ghi789...",
    "priority": 0,
    "timeout": 3600,
    "platform_properties": {
      "cpu_count": "4",
      "memory_kb": "8192"
    },
    "load_timestamp": 1640995200,
    "insert_timestamp": 1640995201,
    "is_finished": false
  }
]
```

#### GET /api/v1/operations/{operation_id}
Returns detailed information about a specific operation.

**Response:**
```json
{
  "operation_id": "op-456",
  "client_operation_id": "client-op-456",
  "stage": "Executing",
  "worker_id": "worker-123",
  "action_digest": "sha256:abc123...",
  "command_digest": "sha256:def456...",
  "input_root_digest": "sha256:ghi789...",
  "priority": 0,
  "timeout": 3600,
  "platform_properties": {
    "cpu_count": "4",
    "memory_kb": "8192"
  },
  "load_timestamp": 1640995200,
  "insert_timestamp": 1640995201,
  "is_finished": false
}
```

### Scheduler Status

#### GET /api/v1/scheduler/status
Returns overall scheduler status and metrics.

**Response:**
```json
{
  "total_workers": 5,
  "active_workers": 4,
  "paused_workers": 1,
  "draining_workers": 0,
  "total_operations": 25,
  "queued_operations": 10,
  "executing_operations": 12,
  "completed_operations": 3,
  "uptime_seconds": 3600
}
```

#### GET /api/v1/scheduler/metrics
Returns detailed scheduler metrics in JSON format.

**Response:**
```json
{
  "workers": {
    "total": 5,
    "active": 4,
    "paused": 1,
    "draining": 0
  },
  "operations": {
    "total": 25,
    "queued": 10,
    "executing": 12,
    "completed": 3
  },
  "uptime_seconds": 3600
}
```

### System Health

#### GET /api/v1/system/health
Returns system health information.

**Response:**
```json
{
  "status": "healthy",
  "timestamp": 1640995200,
  "uptime_seconds": 3600
}
```

## Error Responses

All endpoints return appropriate HTTP status codes:

- `200 OK`: Success
- `404 Not Found`: Resource not found
- `500 Internal Server Error`: Server error

Error responses include a JSON body with error details:

```json
{
  "error": "Worker worker-123 not found"
}
```

## Example Usage

### Get all workers
```bash
curl http://localhost:50062/api/v1/workers
```

### Get operations for a specific worker
```bash
curl "http://localhost:50062/api/v1/operations?worker_id=worker-123"
```

### Get scheduler status
```bash
curl http://localhost:50062/api/v1/scheduler/status
```

### Get queued operations only
```bash
curl "http://localhost:50062/api/v1/operations?stage=queued"
```

## Security Considerations

The monitoring API provides sensitive information about the scheduler state. It should be:

1. **Served on a separate port** from public-facing services
2. **Protected by authentication/authorization** in production environments
3. **Not exposed to the public internet** without proper security measures

## Integration with Dashboards

The monitoring API is designed to be easily integrated with monitoring dashboards like:

- Grafana
- Prometheus (via custom exporters)
- Custom web dashboards
- Monitoring tools like Datadog, New Relic, etc.

The JSON responses are structured to be easily consumed by dashboard frameworks and monitoring systems. 