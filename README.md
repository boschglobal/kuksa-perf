# Eclipse Kuksa databroker-perf

Performance measurement app for KUKSA databroker.

```
[00:00:09] Group: Frame 1 | Cycle(ms): 10 | Current latency: 0.868 ms [===========================================================================================================]    1000/1000    iterationsSummary:

Group: Frame 1 | Cycle time(ms): 10
  API: KuksaValV2
  Elapsed time: 9.99 s
  Rate limit: 10 ms between iterations
  Sent: 1000 iterations * 1 signals = 1000 updates
  Skipped: 10 updates
  Received: 990 updates
  Fastest:   0.192 ms
  Slowest:   1.162 ms
  Average:   0.817 ms

Latency histogram:
    0.220 ms [1    ] |
    0.308 ms [6    ] |∎
    0.396 ms [6    ] |∎
    0.484 ms [6    ] |∎
    0.572 ms [14   ] |∎∎
    0.660 ms [81   ] |∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
    0.748 ms [263  ] |∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
    0.836 ms [310  ] |∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
    0.924 ms [246  ] |∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎∎
    1.012 ms [50   ] |∎∎∎∎∎∎∎∎∎
    1.100 ms [5    ] |
    1.188 ms [2    ] |

Latency distribution:
  10% in under 0.694 ms
  25% in under 0.747 ms
  50% in under 0.830 ms
  75% in under 0.892 ms
  90% in under 0.942 ms
  95% in under 0.976 ms
  99% in under 1.040 ms

```

# Local Setup

## Build databroker-perf binary

```
cargo build --release
```

## Start databroker (Docker)

```
docker run -it --rm -p 55555:55555 ghcr.io/eclipse-kuksa/kuksa-databroker:main --insecure --enable-databroker-v1
```

If running on MacOS:

```
docker run -it --rm -p 55556:55556 ghcr.io/eclipse-kuksa/kuksa-databroker:main --insecure --enable-databroker-v1 --port 55556
```

## Start databroker (Binary)

Use binary from [kuksa-databroker repository](https://github.com/eclipse-kuksa/kuksa-databroker)

```
cargo build --release
```

```
./target/release/databroker --vss data/vss-core/vss_release_4.0.json --enable-databroker-v1
```

If running on MacOS:

```
./target/release/databroker --vss data/vss-core/vss_release_4.0.json --enable-databroker-v1 --port 55556
```

## Usage databroker-perf

```
Usage: databroker-perf [OPTIONS]

Options:
  -r, --run-seconds <RUN_SECONDS>  Number of seconds to run [default: 8]
      --api <API>                  Api of databroker [default: kuksa.val.v1] [possible values: kuksa.val.v1, kuksa.val.v2, sdv.databroker.v1]
      --host <HOST>                Host address of databroker [default: http://127.0.0.1]
      --port <PORT>                Port of databroker [default: 55555]
      --skip-seconds <ITERATIONS>  Seconds to run (skip) before measuring the latency [default: 4]
      --detailed-output              Print more details in the summary result
      --config <FILE>              Path to configuration file
      --run-forever                Run the measurements forever (until receiving a shutdown signal)
  -v, --verbosity <LEVEL>          Verbosity level. Can be one of ERROR, WARN, INFO, DEBUG, TRACE [default: WARN]
  -h, --help                       Print help
  -V, --version                    Print version
```

```
./target/release/databroker-perf [OPTIONS]
```

## Default test result output

By default, the results output will be shown like this:
```
[00:00:08] Group: Frame A | Cycle(ms): 10 | Current latency: 0.725 ms [==============================================================================================================]       8/8       seconds
[00:00:07] Group: Frame B | Cycle(ms): 20 | Current latency: 0.607 ms [==============================================================================================================]       8/8       seconds
[00:00:07] Group: Frame C | Cycle(ms): 30 | Current latency: 0.709 ms [==============================================================================================================]       8/8       seconds

Summary:
  API: KuksaValV1
  Run seconds:         8
  Skipped run seconds: 4

Group: Frame A | Cycle time(ms): 10
  Average:     0.710 ms
  95% in under 0.851 ms


Group: Frame B | Cycle time(ms): 20
  Average:     0.614 ms
  95% in under 0.738 ms


Group: Frame C | Cycle time(ms): 30
  Average:     0.701 ms
  95% in under 0.855 ms

```

For a detailed output of the results, please enable the corresponding flag like:

```
./target/release/databroker-perf --detailed-output
```

## Group config file

Databroker-perf creates two new gRPC channels for each group: one for the provider and one for the subscriber.
Each provider will update its group signal values to the Databroker at the cycle time specified (in milliseconds) in the JSON configuration file provided.

i. e.
```
{
  "groups": [
    {
      "group_name": "Frame 1",
      "cycle_time_ms": 10,
      "signals": [
        {
          "path": "Vehicle.Speed"
        }
      ]
    },
    {
      "group_name": "Frame 2",
      "cycle_time_ms": 20,
      "signals": [
        {
          "path": "Vehicle.IsBrokenDown"
        },
        {
          "path": "Vehicle.IsMoving"
        },
        {
          "path": "Vehicle.AverageSpeed"
        }
      ]
    }
  ]
}
```

## Example with config file

```
./target/release/databroker-perf --config configs/config_group_10.json
```

If running on MacOS:

```
./target/release/databroker-perf --config configs/config_group_10.json --port 55556
```

## Example with API

```
./target/release/databroker-perf --api sdv.databroker.v1 --config configs/config_group_10.json
```

If running on MacOS:

```
./target/release/databroker-perf --api sdv.databroker.v1 --config configs/config_group_10.json --port 55556
```

## Contributing

Please refer to the [Kuksa Contributing Guide](CONTRIBUTING.md).

## License

Kuksa Databroker Perf tool is provided under the terms of the [Apache Software License 2.0](LICENSE).

## Contact

Please feel free to create [GitHub Issues](https://github.com/eclipse-kuksa/kuksa-perf/issues) for reporting bugs and/or ask questions in our [Gitter chat room](https://matrix.to/#/#kuksa-val_community:gitter.im).
