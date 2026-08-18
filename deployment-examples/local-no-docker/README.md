# Local setup without Docker

This example runs three NativeLink processes on one machine.

| Process | Config | Ports |
| --- | --- | --- |
| CAS and AC server | `cas.json5` | 50051 |
| Scheduler server | `scheduler.json5` | 50052 (clients), 50061 (workers) |
| Local worker | `worker.json5` | 50071 (health) |

The scheduler and the worker hold no local CAS. Both read the CAS through a
gRPC store that points at port 50051. The worker also writes action results
through a gRPC AC store on the same port.

## Compression

The CAS server advertises zstd wire compression with
`capabilities.remote_cache_compression`. The gRPC CAS stores of the scheduler
and the worker send zstd with `experimental_remote_cache_compression`. The
gRPC AC store does not compress. Compression applies to CAS blobs only.

## Run it

```sh
./deployment-examples/local-no-docker/run.sh
```

The script builds the `nativelink` binary and starts the three processes. To
use a binary that exists, set `NATIVELINK_BIN`.

## Use it from Bazel

```sh
bazel test --remote_cache=grpc://127.0.0.1:50051 \
           --remote_executor=grpc://127.0.0.1:50052 \
           --remote_cache_compression \
           --remote_default_exec_properties=cpu_count=1 \
           //some:target
```

## Reset

All data is in `/tmp/nativelink-local`. Stop the processes and delete that
directory.
