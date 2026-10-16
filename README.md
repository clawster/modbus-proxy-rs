# Modbus TCP proxy

Many modbus devices support only one or very few clients. This proxy acts as a
bridge between the client and the modbus device. It can be seen as a layer 7
reverse proxy.
This allows multiple clients to communicate with the same modbus device.

When multiple clients are connected, cross messages are avoided by serializing
communication on a first come first served REQ/REP basis.

This project is the [Rust][rust] version of the [Python][python] based
[modbus-proxy][modbus-proxy-py] project.

I did it because it fitted my personal goal of exercising with the [Rust][rust]
programming language and it's async based [tokio] library.

The goal was to produce a robust, highly concurrent server with a low
memory footprint.

## Installation

```bash
$ cargo install modbus-proxy-rs
```

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

Log levels can be adjusted by setting the `RUST_LOG` environment variable (default is `warn`):

```bash
$ RUST_LOG=debug modbus-proxy-rs -c ./modbus-config.yml
```

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

## Credits

### Development Lead

* Tiago Coutinho <coutinhotiago@gmail.com>

### Contributors

None yet. Why not be the first?

[rust]: https://www.rust-lang.org/
[python]: https://python.org/
[modbus-proxy-py]: https://github.com/tiagocoutinho/modbus-proxy
[tokio]: https://tokio.rs/
