# zcash-devtool with PostgreSQL support

A CLI app for working with Zcash transactions and the Zcash blockchain,
including stateless wallet functionality. It supports both SQLite and PostgreSQL
database backends.

## Security Warnings

**DO NOT USE THIS IN PRODUCTION!!!**

This app has not been written with security in mind. It does however have
affordances such as encryption of the mnemonic seed phrases that should make it
viable for small scale experimentation, at your own risk.

## Quick Start with PostgreSQL

This walkthrough uses the publicly-known Zec.rocks Node Reward Wallet UFVK
to demonstrate the postgres backend.

### Prerequisites

- Rust toolchain (install via [rustup](https://rustup.rs))
- PostgreSQL running locally

### 1. Create a PostgreSQL database

```
createdb zcash_devtool
```

### 2. Build with postgres support

```
cargo build --release --features postgres
```

### 3. Initialize a view-only wallet

Import the Zec.rocks Node Reward Wallet UFVK with birthday height 3066155.
A local wallet directory (`-w`) is still needed for the block cache and Tor
state, even when using postgres for the wallet database.

```
cargo run --release --features postgres -- \
  wallet -w /tmp/zcash-pg-test \
  --database "postgres://localhost/zcash_devtool" \
  init-fvk \
  --name "ZecRocks Node Rewards" \
  --fvk "uview1fl9k4zu4p52u7mzkg3d93yyfh6xhqegcwh7nqadkl49d3gm47tl2cw50lguaveyg0yamm3lpymr4zfv56y4lqsfyacw49r2fz936z34pcy0wyt0vmdhp287gwh3vw4s3dcvd54wkju90548knm0hg6npsq8yasky705hxskp8c3h3s24h4dtwmxwmyt3ccf26qhcj3vwmglj652z7ug3py8k0rkl6x3wxrwjgs2ztu25280rr8jc47fc9ercw9azjud7m0cmahmf32tea8kdnyn0msgtq8lxneyucf5ht6dg779uk6mmaaweutx4h450slfffgjlf02p0k5kjydzgze0xhrdtv3kz6kncv9sfrn3rx7pmhk5yd22v8zxuz2wdk07q3c90yem3" \
  --birthday 3066155 \
  -s zecrocks
```

This prints a wallet UUID like `Created wallet: 3721d10c-eb66-4ee4-85a3-458de406688c`.

### 4. Sync the wallet

```
cargo run --release --features postgres -- \
  wallet -w /tmp/zcash-pg-test \
  --database "postgres://localhost/zcash_devtool" \
  sync -s zecrocks
```

The sync connects to the Zec.rocks lightwalletd server over Tor, downloads
compact blocks, and scans them into the postgres database. Press Ctrl-C to
stop; re-running `sync` resumes where it left off.

### 5. Check balance

```
cargo run --release --features postgres -- \
  wallet -w /tmp/zcash-pg-test \
  --database "postgres://localhost/zcash_devtool" \
  balance
```

### 6. List accounts

```
cargo run --release --features postgres -- \
  wallet -w /tmp/zcash-pg-test \
  --database "postgres://localhost/zcash_devtool" \
  list-accounts
```

### 7. List transactions

```
cargo run --release --features postgres -- \
  wallet -w /tmp/zcash-pg-test \
  --database "postgres://localhost/zcash_devtool" \
  list-tx
```

### Multi-wallet support

The postgres backend supports multiple wallets in a single database. When only
one wallet exists, it is selected automatically. With multiple wallets, specify
which one to use:

```
cargo run --release --features postgres -- \
  wallet -w /tmp/zcash-pg-test \
  --database "postgres://localhost/zcash_devtool" \
  --wallet-id "3721d10c-eb66-4ee4-85a3-458de406688c" \
  balance
```

## SQLite Usage

The default database backend is SQLite, which stores everything in the wallet
directory.

To obtain the help docs:
```
cargo run --release -- --help
```

To create a new empty testnet wallet:
```
cargo run --release -- wallet -w <wallet_dir> init --name "<account_name>" -i <identity_file> -n test
cargo run --release -- wallet -w <wallet_dir> sync
```

Note: The `-i` (identity) parameter specifies an age identity file for
encrypting the mnemonic phrase. The file will be generated if it doesn't exist.

For mainnet with a lightwallet server:
```
cargo run --release -- wallet -w <wallet_dir> init --name "<account_name>" -i <identity_file> -n main -s zecrocks
cargo run --release -- wallet -w <wallet_dir> sync -s zecrocks
```

Whenever you update the `zcash_client_sqlite` dependency, run migrations:
```
cargo run --release -- wallet -w <wallet_dir> upgrade
```

### Debug logging

```
RUST_LOG=debug cargo run --release -- wallet -w <wallet_dir> <command>
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
