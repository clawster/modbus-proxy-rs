# Modbus TCP proxy

Many modbus devices support only one or very few clients. This proxy acts as a
bridge between the client and the modbus device. It can be seen as a layer 7
reverse proxy.
This allows multiple clients to communicate with the same modbus device.

When multiple clients are connected, cross messages are avoided by serializing
communication on a first come first served REQ/REP basis.

This is a fork of [tiagocoutinho/modbus-proxy-rs][upstream], the [Rust][rust]
version of the [Python][python] based [modbus-proxy][modbus-proxy-py] project.
On top of the original it adds:

* **Register descriptions in the log**: with a register map, every request is
  logged with register names, decoded values and the client that made it. A
  map for [Sigenergy](#sigenergy) inverters is included.
* **Robustness**: timeouts on device and client I/O so a stalled device no
  longer leaves sockets in `CLOSE_WAIT` (upstream PR #12, issue #7), accept
  errors such as running out of file descriptors are retried instead of
  crashing, and a busy listen port is reported instead of panicking.
* **Docker Compose** setup configured from a `.env` file.
* Up to date dependencies (clap instead of the unmaintained structopt, clean
  `cargo audit`) and tests with 100% line coverage.

## Installation

Build from source (needs Rust 1.99 or newer):

```bash
$ cargo install --git https://github.com/clawster/modbus-proxy-rs
```

or clone the repository and run `cargo build --release`. Note that
`cargo install modbus-proxy-rs` installs the original crate from crates.io,
without the changes of this fork. See [Docker](#docker) to run it in a
container instead.

## Running the server

First, you will need write a configuration file where you specify for each
modbus device you which to control:

* modbus connection (the modbus device url)
* listen interface (to which url your clients should connect)

Configuration files can be written in YAML (*.yml* or *.yaml*) or
TOML (*.toml*).

Suppose you have a PLC modbus device listening on *plc1.acme.org:502* and
you want your clients to connect to your machine on port 9000. A YAML
configuration would look like this:

```yaml
devices:
- modbus:
    url: plc1.acme.org:502     # device url (mandatory)
  listen:
    bind: 0:9000               # listening address (mandatory)
```

Assuming you saved this file as `modbus-config.yml`, start the server with:

```bash
$ modbus-proxy-rs -c ./modbus-config.yml
```

Now, instead of connecting your client(s) to `plc1.acme.org:502` you just need to
tell them to connect to `*machine*:9000` (where *machine* is the host where
modbus-proxy is running).

Note that the server is capable of handling multiple modbus devices. Here is a
configuration example for 2 devices:

```yaml
devices:
- modbus:
    url: plc1.acme.org:502
  listen:
    bind: 0:9000
- modbus:
    url: plc2.acme.org:502
  listen:
    bind: 0:9001
```

## Logging

Log levels can be adjusted by setting the `RUST_LOG` environment variable
(default is `info`):

```bash
$ RUST_LOG=debug modbus-proxy-rs -c ./modbus-config.yml
```

Every `status_interval` seconds (default 120, `0` disables it) each device
logs a summary: the number of connected clients and the reads, writes and
errors since the last summary, then one line per connected client with its
totals:

```yaml
status_interval: 120
devices:
  ...
```

```
INFO modbus_proxy_rs::status] sigenergy: 2 clients, last 2m00s: 1240 reads, 1 writes, 0 errors
INFO modbus_proxy_rs::status] sigenergy: client 10.0.0.5:51234 connected 3h12m05s: 69120 reads, 0 writes, 0 errors
INFO modbus_proxy_rs::status] sigenergy: client 10.0.0.6:40112 connected 5m10s: 3100 reads, 1 writes, 0 errors
```

Errors are exception replies and requests the device didn't answer.

## Register descriptions

Give a device a register map and every request is logged in words, with
register names, decoded values (gain and unit applied) and descriptions:

```yaml
devices:
- modbus:
    url: 10.0.0.30:502
    name: sigenergy                       # optional, defaults to the url
    registers: registers/sigenergy.json   # optional
  listen:
    bind: 0.0.0.0:10502
```

```
DEBUG modbus_proxy_rs::registers] 10.0.0.5:51234 -> sigenergy: unit 247 read 30005 plant_grid_sensor_active_power = -1.234 kW (Grid Active Power (>0 buy from grid; <0 sell to grid))
INFO modbus_proxy_rs::registers] 10.0.0.5:51234 -> sigenergy: unit 247 write 40000 plant_start_stop = 1 (Start/Stop (0: Stop 1: Start))
```

Each line starts with the address of the client that sent the request and
the device `name`.

Reads (functions 3/4), writes (6/16) and exception replies are described.
The lines are logged under the `modbus_proxy_rs::registers` target: reads
at `debug` (clients poll constantly), writes at `info` and exception
replies at `warn`. To see reads as well use
`RUST_LOG=info,modbus_proxy_rs::registers=debug`.

A register map is a JSON file listing `address`, `count`, `type` (`u16`,
`s16`, `u32`, `s32`, `u64`, `string`), `gain`, and optionally `unit`, `name`
and `description` per register; see [registers/](./registers). The proxy
refuses to start if a configured map can't be loaded.

### Sigenergy

[registers/sigenergy.json](./registers/sigenergy.json) is generated from the
register definitions of the [Sigenergy-Local-Modbus][sigenergy] Home
Assistant integration (MIT), included as a git submodule:

```bash
$ git submodule update --init
$ python3 -I tools/gen_register_map.py \
    Sigenergy-Local-Modbus/custom_components/sigen/modbusregisterdefinitions.py \
    registers/sigenergy.json
```

The generator only parses the Python source, it doesn't import it, so Home
Assistant isn't needed. It also applies corrections checked against the
Sigenergy protocol document (e.g. the inverter power factor at 31023 is
signed), listed in `CORRECTIONS` in the script.

## Docker

This project ships with a [Dockerfile](./Dockerfile) which you can use as a
base to launch modbus-proxy inside a docker container.

### Docker Compose

The easiest way is the supplied [compose.yml](./compose.yml). It is configured
from a `.env` file next to it, from which compose generates the proxy
configuration (no config file needed):

```bash
$ cp .env.example .env    # then set MODBUS_DEVICE and PROXY_PORT
$ docker compose build
$ docker compose up -d
$ docker compose logs -f
```

| Variable        | Example          | Meaning                                  |
|-----------------|------------------|------------------------------------------|
| `MODBUS_DEVICE` | `10.0.0.30:502`  | modbus device to proxy (`host:port`)     |
| `PROXY_PORT`    | `10502`          | port clients connect to                  |
| `MODBUS_NAME`   | `sigenergy`      | device name in the register log (defaults to `MODBUS_DEVICE`) |
| `REGISTER_MAP`  | `/etc/modbus-proxy/registers/sigenergy.json` | register map for the log (shipped in the image) |
| `STATUS_INTERVAL` | `120`          | seconds between status summaries, `0` disables |
| `RUST_LOG`      | `info`           | log level (`debug` dumps requests/replies) |

Clients connect to `<your-hostname/ip>:$PROXY_PORT`. After code changes,
rebuild and restart with `docker compose up -d --build`. To proxy more than
one device, add entries to the `configs` section of `compose.yml`, each with
its own listen port, and publish those ports.

### Plain Docker

First, build the docker image with:

```bash
$ docker build -t modbus-proxy .
```

Assuming you have prepared a `config.yml` in the current directory:

```yaml
devices:
- modbus:
    url: plc1.acme.org:502
  listen:
    bind: 0.0.0.0:5020
```

The supplied docker image by default runs the command `/modbus-proxy-rs -c /etc/modbus-proxy.yml`.
Therefore, running launching a container is as simple as:

```bash
docker run --init --rm -p 5020:5020 -v $PWD/config.yml:/etc/modbus-proxy.yml modbus-proxy
```

You can supply a different configuration path (ex: `/config.yml`):

```bash
docker run --init --rm -p 5020:5020 -v $PWD/config.yml:/config.yml modbus-proxy -c /config.yml
```

Now you should be able to access your modbus device through the modbus-proxy by
connecting your client(s) to `<your-hostname/ip>:5020`.

Note that for each modbus device you add in the configuration file you need
to publish the corresponding bind port on the host
(`-p <host port>:<container port>` argument).

The container runs as an unprivileged user, so bind ports above 1024 inside
the container (e.g. `0.0.0.0:5020`) and map them to any host port you like;
binding `:502` inside the container fails with `Permission denied`.

## Development

```bash
$ cargo test            # unit tests, including real loopback sockets
$ cargo clippy --all-targets
$ ./coverage.sh         # line/region coverage report (needs llvm-tools and jq)
$ ./coverage.sh --show  # ... with annotated source
```

The code is split by the path a request takes: `bridge.rs` (clients) sends it
over a channel to `device.rs` (the device connection); `frame.rs` reads Modbus
TCP frames, `modbus.rs` and `registers.rs` describe them, `settings.rs` is the
configuration format and `server.rs` ties it together.

## License

Copyright (C) 2020 Tiago Coutinho, and the contributors of this fork.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](./LICENSE).

The Sigenergy register map is derived from the
[Sigenergy-Local-Modbus][sigenergy] integration, MIT licensed, Copyright (c)
2025 Andrei Ignat; see [registers/LICENSE.sigenergy](./registers/LICENSE.sigenergy).
The Docker image ships both license texts.

This project is not affiliated with or endorsed by Sigenergy Technology Co.,
Ltd.

## Credits

* Original project and author: [modbus-proxy-rs][upstream] by Tiago Coutinho
  <coutinhotiago@gmail.com>
* Upstream contributors: Ingmar Stein (reference counting fix, smaller
  binaries), Daniel Gruber (shutdown on Ctrl+C/SIGTERM), and schakko, whose
  [PR #12][pr12] the I/O timeouts are based on
* Sigenergy register definitions: [Sigenergy-Local-Modbus][sigenergy]

[upstream]: https://github.com/tiagocoutinho/modbus-proxy-rs
[pr12]: https://github.com/tiagocoutinho/modbus-proxy-rs/pull/12
[rust]: https://www.rust-lang.org/
[python]: https://python.org/
[modbus-proxy-py]: https://github.com/tiagocoutinho/modbus-proxy
[tokio]: https://tokio.rs/
[sigenergy]: https://github.com/TypQxQ/Sigenergy-Local-Modbus
