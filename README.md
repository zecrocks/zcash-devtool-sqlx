# zcash-devtool

This repository contains a CLI app for working with Zcash transactions and the
Zcash blockchain, including stateless Zcash wallet functionality based upon the
`zcash_client_backend` and `zcash_client_sqlite` crates. It is built by
developers, for developers for use in prototyping Zcash functionality, and
should not be considered production-ready. The command-line API that this tool
exposes can and will change at any time and without warning.

## Security Warnings

**DO NOT USE THIS IN PRODUCTION!!!**

This app has not been written with security in mind. It does however have affordances
such as encryption of the mnemonic seed phrases that should make it viable for small
scale experimentation, at your own risk.

## Usage

No binary artifacts are provided for this crate; it is generally used via
`cargo run` as follows:

To obtain the help docs:
```
cargo run --release -- --help
```
To obtain the help for a specific command (in this case, `init`)
```
cargo run --release -- --help init
```

To create a new empty testnet wallet:
```
cargo run --release -- wallet -w <wallet_dir> init --name "<account_name>" -i <identity_file> -n test
cargo run --release -- wallet -w <wallet_dir> sync
```

Note: The `-i` (identity) parameter specifies an age identity file for encrypting the mnemonic phrase. The file will be generated if it doesn't exist.

See the help docs for `init` for additional information, including for how to
initialize a mainnet wallet. Initializing a mainnet wallet will require
specifying a mainnet lightwallet server, e.g.
```
cargo run --release -- wallet -w <wallet_dir> init --name "<account_name>" -i <identity_file> -n main -s zecrocks
cargo run --release -- wallet -w <wallet_dir> sync -s zecrocks
```

Whenever you update the `zcash_client_sqlite` dependency, in order to run
necessary migrations:
```
cargo run --release -- wallet -w <wallet_dir> upgrade
```

If you want to run with debug or trace logging:
```
RUST_LOG=debug cargo run --release -- wallet -w <wallet_dir> <command>
```
### Video tutorial of Zcash Devtool
Kris Nuttycombe (@nuttycom) presented this tool during ZconVI. The session is available
on Youtube [here](https://www.youtube.com/watch?v=5gvQF5oFT8E)

[![Youtube preview of the ZconVI presentation Zcash-devtool: the Zcash development multitool](https://img.youtube.com/vi/5gvQF5oFT8E/0.jpg)](https://www.youtube.com/watch?v=5gvQF5oFT8E)

The code developed in this demo resulted in [this](https://github.com/zcash/zcash-devtool/pull/86) pull request.

## Docker Deployment

The HTTP indexer service can be run via Docker Compose.

### Quick Start

```
docker compose up -d
curl http://localhost:8080/health
```

### Configuration

All settings are configurable via environment variables or a `.env` file:

| Variable | Default | Description |
|---|---|---|
| `BIND_PORT` | `8080` | Host port to expose |
| `RUST_LOG` | `info` | Log level (`debug`, `trace`, etc.) |
| `ZCASH_SERVER` | `zecrocks` | Mainnet lightwalletd server |
| `ZCASH_TESTNET_SERVER` | `ecc` | Testnet lightwalletd server |
| `ZCASH_CONNECTION` | `direct` | Connection mode: `direct`, `tor`, or `socks5://host:port` |
| `ZCASH_SYNC_INTERVAL` | `60` | Seconds between sync cycles |

Example `.env` file:
```
RUST_LOG=debug
ZCASH_SERVER=zecrocks
BIND_PORT=9090
```

### Build Notes

- The initial build compiles the full Zcash/librustzcash dependency tree and will take a long time. Subsequent rebuilds with only source changes are fast thanks to cached dependency layers.
- Recommend at least 4 GB RAM for the Docker build.
- Wallet data is persisted in a named Docker volume (`zcash-data`). It survives `docker compose down` and restarts.

### Commands

```
docker compose build        # Build the image
docker compose up -d        # Start in background
docker compose logs -f      # Follow logs
docker compose down         # Stop (data persists)
docker compose down -v      # Stop and delete data volume
```

## Example: Register a UFVK via the HTTP API

```
curl -X POST http://localhost:8080/ufvks \
  -H "Content-Type: application/json" \
  -d '{
    "ufvk": "uview1fl9k4zu4p52u7mzkg3d93yyfh6xhqegcwh7nqadkl49d3gm47tl2cw50lguaveyg0yamm3lpymr4zfv56y4lqsfyacw49r2fz936z34pcy0wyt0vmdhp287gwh3vw4s3dcvd54wkju90548knm0hg6npsq8yasky705hxskp8c3h3s24h4dtwmxwmyt3ccf26qhcj3vwmglj652z7ug3py8k0rkl6x3wxrwjgs2ztu25280rr8jc47fc9ercw9azjud7m0cmahmf32tea8kdnyn0msgtq8lxneyucf5ht6dg779uk6mmaaweutx4h450slfffgjlf02p0k5kjydzgze0xhrdtv3kz6kncv9sfrn3rx7pmhk5yd22v8zxuz2wdk07q3c90yem3",
    "birthday": 3066155,
    "name": "ZecRocks Node Rewards"
  }'
```

The response includes a wallet `id`. Use it to fetch transaction history once the wallet has synced:

```
curl http://localhost:8080/ufvks/<wallet-id>/transactions
```

## Documentation

For a step-by-step guide for how to get started using these tools, see [this
walkthrough](doc/walkthrough.md).

## License

All code in this workspace is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
