# Benchmark harness

Manual harnesses outside the gate. Loopback measures software limits, not a physical network; interpret results with
the [measurement definitions](MEASUREMENTS.md) and keep reports and raw evidence outside the repository.

| Task                     | Measures                                                                   |
| ------------------------ | -------------------------------------------------------------------------- |
| `mise run bench-ui`      | UI frame delivery with default stage lengths against the E2E fleet.        |
| `mise run bench-session` | Browser resources retained across repeated tests against the E2E fleet.    |
| `mise run bench-wire`    | Go wire codec cost.                                                        |
| `mise run stress`        | Server saturation envelope (Unix).                                         |

## Native terminal

```sh
cd go && go test ./cmd/graphite-meter-client -run '^$' -bench '^BenchmarkTUI' -benchmem
```

The frame workload retains four servers and 480 points per trace at 80×24, 120×40 and 160×50. Animation frames reuse
unchanged samples; sample frames update throughput and latency. These measure Go rendering time and allocations,
excluding terminal-emulator drawing and network traffic.
