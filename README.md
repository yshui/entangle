# entangle
Spooky input device action at a distance

## Build

Required dependencies:

* libudev (found in systemd-libs, or libudev1)
* rust
* cargo

Build command:

```
cargo build
```

## Usage

First, you need to pair your server, which hosts the input devices; and the client, which receives the input.

To do that, run:

```
sudo cargo run --bin pair -- -l
```

on the server, which will tell you the server port. Then run

```
sudo cargo run --bin pair -- -s <server ip>:<server port>
```

on the client machine, and follow the instructions.

After the machines are paired, you just need to start the server and client daemons with:

```
sudo cargo run --bin daemon -- server
```

and

```
sudo cargo run --bin daemon -- client -s <server ip>
```

respectively. Input will be forwarded as long as the daemons are running.

## TODOs

* Detect server/client death, and automatic reconnect.
* Daemonization

## Known bugs

* Hot-plugging devices doesn't work currently.
