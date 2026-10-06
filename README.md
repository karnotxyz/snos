<div align="center">
  <img src="./docs/images/SNOS.png" height="400" width="500">
  
  ### ✨ SNOS ✨
  
  A Rust Library for running the [Starknet OS](https://github.com/starkware-libs/cairo-lang/blob/master/src/starkware/starknet/core/os/os.cairo).

  [Report Bug](https://github.com/keep-starknet-strange/snos/issues/new?assignees=&labels=bug&projects=&template=bug_report.md&title=bug%3A+) · [Request Feature](https://github.com/keep-starknet-strange/snos/issues/new?labels=enhancement&title=feat%3A+)

  [![Check Workflow Status](https://github.com/keep-starknet-strange/snos/actions/workflows/check.yml/badge.svg)](https://github.com/keep-starknet-strange/snos/actions/workflows/check.yml)
[![license](https://img.shields.io/github/license/keep-starknet-strange/snos)](/LICENSE)
[![pr-welcome]](#-contributing)

[pr-welcome]: https://img.shields.io/static/v1?color=blue&label=PRs&style=flat&message=welcome

</div>

## Table of Contents
- [Table of Contents](#table-of-contents)
- [📖 About](#-about)
- [🛠️ Getting Started](#️-getting-started)
  - [Prerequisites](#prerequisites)
  - [Installation](#installation)
- [🧪 Running Tests](#-running-tests)
  - [Run Tests](#run-tests)
  - [Reset Tests](#reset-tests)
- [🚀 Usage](#-usage)
  - [Adding SNOS as a Dependency](#adding-snos-as-a-dependency)
  - [Using the **prove_block** Binary](#using-the-prove_block-binary)
- [🤝 Related Projects](#-related-projects)
- [📚 Documentation](#-documentation)
- [📜 License](#-license)


## 📖 About

[Starknet OS](https://github.com/starkware-libs/cairo-lang/blob/master/src/starkware/starknet/core/os/os.cairo) is a [Cairo](https://www.cairo-lang.org/) program designed to prove the integrity of state transitions between blocks on Starknet.

By re-executing transactions from a block and verifying consistency, it produces a [PIE](https://github.com/starkware-libs/cairo-lang/blob/a86e92bfde9c171c0856d7b46580c66e004922f3/src/starkware/cairo/lang/vm/cairo_pie.py#L219-L225) (Program Independent Execution) result. This PIE can be used to generate a STARK proof of integrity, which, if accepted by Starknet L1 verifiers, confirms block validity and updates the Starknet state root in the [StarknetCore contract](https://etherscan.io/address/0xc662c410c0ecf747543f5ba90660f6abebd9c8c4#code).

## 🛠️ Getting Started

### Prerequisites

Ensure you have the following dependencies installed:
- [Rust 1.76.0 or newer](https://www.rust-lang.org/tools/install)

#### Optional
- [pyenv](https://github.com/pyenv/pyenv-installer?tab=readme-ov-file#install) (recommended for managing Python versions and setting up environment)

- [rclone](https://rclone.org/install/) (recommended for downloading Pathfinder's database and being able to quickly use [`prove_block`](#using-the-prove_block-binary) binary)

### Installation

1. **Clone the Repository**

Clone this repository and its submodules:
   ```bash
   git clone https://github.com/keep-starknet-strange/snos.git --recursive
  ``` 

#### Install project dependencies
In order to compile the Starknet OS Cairo program, you’ll need the Cairo compiler:

- Follow the [Cairo documentation](https://docs.cairo-lang.org/quickstart.html)
- Or simply run:
```bash
./setup-scripts/setup-cairo.sh
```

This will create a virtual environment and download needed dependencies to compile cairo programs. You will need to activate it to compile Cairo programs.

## 🧪 Running Tests

SNOS includes comprehensive end-to-end tests for PIE generation. Tests can be run using the convenient Makefile or directly with cargo.

### Quick Start - Using Makefile

```bash
# Run quick integration tests (no RPC required)
make test-quick

# Check test environment
make env-check

# Run full e2e tests (requires RPC endpoint)
make test-e2e

# See all available test commands
make help
```

### Test Categories

- **Quick Integration Tests**: Fast tests that verify workspace integration without requiring external RPC
- **E2E PIE Generation Tests**: Complete workflow tests that generate actual PIE files
- **Error Handling Tests**: Comprehensive error scenario testing
- **Performance Tests**: Timing and resource usage validation

### Using Different RPC Endpoints

```bash
# Test against local Pathfinder instance
make test-e2e RPC_URL=http://localhost:9545

# Test against Sepolia testnet
make test-sepolia

# Test with verbose output
make test-pie VERBOSE=true
```

### Legacy Test Setup

For the original Cairo test environment:

```bash
# Activate Cairo environment
source ./snos-env/bin/activate

# Set up tests
./scripts/setup-tests.sh

# Run workspace tests
cargo test --workspace

# Reset test environment if needed
./scripts/reset-tests.sh
```

##  🚀 Usage 
### Adding SNOS as a dependency

You can add the following to your rust project's `Cargo.toml`:

```toml
starknet-os = { git = "https://github.com/keep-starknet-strange/snos", rev = "662d1706f5855044e52ebf688a18dd80016c8700" }
```

### Using the `prove_block` Binary

To execute correctly, SNOS requires detailed block information, including:

- **State changes**: Information about new classes, contracts, and any modifications to contract storage. (See [StateDiff](https://github.com/xJonathanLEI/starknet-rs/blob/5c676a64031901b5a203168fd8ef8d6b40a5862f/starknet-core/src/types/codegen.rs#L1723-L1737))
- **Storage proofs**: [Merkle Proofs](https://www.quicknode.com/docs/starknet/pathfinder_getProof) from both class and contract tries, needed for validating that updated values match the global state root.
- **Transaction execution Information**: Data on [calls, subcalls](https://github.com/starkware-libs/sequencer/blob/7aa546acde88c94825992501662788e716db5fe0/crates/blockifier/src/transaction/objects.rs#L168-L183), and specific program counters visited ([VisitedPCs](https://github.com/starkware-libs/sequencer/blob/7aa546acde88c94825992501662788e716db5fe0/crates/blockifier/src/state/cached_state.rs#L34-L35)) during execution.

The `prove_block` binary handles this entire process by collecting, formatting, and feeding the necessary data into the OS, ensuring the correct `OSInput` is passed for execution.

To accomplish this, it queries the required information from a full node. Currently, Pathfinder is the only full node implementing all the necessary RPC methods, so a synced [Pathfinder](https://github.com/eqlabs/pathfinder) instance running as an [**archive node**](https://github.com/eqlabs/pathfinder?tab=readme-ov-file#state-trie-pruning) (to provide access to storage proofs) is required to execute this binary successfully.

For example, you can run Pathfinder executing:

```bash
PATHFINDER_ETHEREUM_API_URL="YOUR_KEY" ./target/release/pathfinder --data-directory /home/herman/pathfinder-data --http-rpc 0.0.0.0:9545 --storage.state-tries archive
```

Once you have a synced full node, you can start generating PIEs of a given block by running:

```bash
cargo run --release -p prove_block -- --block-number 200000 --rpc-provider http://0.0.0.0:9545
```

## 🤝 Related Projects

- [cairo compiler](https://github.com/starkware-libs/cairo): A blazing fast compiler for Cairo, written in Rust
- [cairo vm](https://github.com/lambdaclass/cairo-vm): A faster and safer implementation of the Cairo VM in Rust
- [blockifier](https://github.com/starkware-libs/sequencer/tree/7218aa1f7ca3fe21c0a2bede2570820939ffe069/crates/blockifier): The transaction-executing component in the Starknet sequencer.
- [pathfinder](https://github.com/eqlabs/pathfinder): A Starknet full node written in Rust
- [madara](https://github.com/madara-alliance/madara): A powerful Starknet client written in Rust.

## 📚 Documentation

### Cairo:
- [The Cairo Book](https://book.cairo-lang.org/)
- [How Cairo Works](https://docs.cairo-lang.org/how_cairo_works/index.html)
- [Cairo – a Turing-complete STARK-friendly CPU architecture](https://eprint.iacr.org/2021/1063)
- [A Verified Algebraic Representation of Cairo Program Execution](https://arxiv.org/pdf/2109.14534)

### Starknet
- [Starknet Docs](https://docs.starknet.io/)
  -  [Starknet State](https://docs.starknet.io/architecture-and-concepts/network-architecture/starknet-state/)
- [MoonsongLabs talk in StarknetCC](https://www.youtube.com/watch?v=xHc_pKXN9h8)

### StarknetOS
- [Pragma Article on os.cairo](https://hackmd.io/@pragma/ByP-iux1T)
- [os.cairo code](https://github.com/starkware-libs/cairo-lang/blob/master/src/starkware/starknet/core/os/os.cairo)


## 📜 License

This project is licensed under the MIT License. See the [LICENSE](./LICENSE) file for details.

## Committed Data Reads (appchain extension)

This fork supports `get_value(root, index)` at
`starknet_keccak("committed_data_v1")`. A value is a full felt, not an Oracle-specific
price. The application must obtain the root from its authenticated storage and
control publication and freshness. The leaf binds `COMMITTED_DATA_V1`, the index
and the value. Dataset roots and witnesses are shared across contracts; they are
not bound to a publisher address. The Cairo OS independently verifies the ordered,
19-level Poseidon path and the returned value.

Supported transaction scope: committed-data reads may only execute within account
transactions. L1-handler transactions must never reach this primitive, either
directly or indirectly through an adapter or another contract. Adapter
contracts, their callers and future upgrades must preserve this restriction. This
is an application-enforced constraint, not a runtime rejection of L1 handlers.
The missing-witness rejection guard covers legacy and current account transactions;
the L1-handler path does not have the equivalent guard and can accept an availability
failure as a reverted transaction. L1-handler support requires that guard and
execution/replay regression coverage before this constraint can be relaxed.

Failure and recovery behavior:

- An ordinary deterministic contract revert is supported: replay must reproduce the
  original execution with the same historical state, configuration and committed
  data. A revert alone does not indicate a committed-data failure.
- If an L1 handler reaches an adapter and its witness is unavailable during
  block production, the current L1-handler path can record an accepted reverted
  transaction. If the witness becomes available for SNOS replay, execution may
  instead succeed, changing receipts, commitments or state. Replay consistency
  checks or Cairo constraints then fail; detecting the mismatch does not repair the
  original block. This can stall proving. L1-handler use remains unsupported even
  when data is available.
- If the original execution was valid and only the proving node lacks data, restore
  the historical witnesses for the exact root and index,
  or restore access to a witness RPC that serves them, then retry the same input.
  Enable `use_committed_data` for replay and proving. Valid witnesses
  resolve the availability failure; all remaining replay and proof checks must
  still pass.
- If the original block recorded an availability-driven L1-handler revert, supplying
  data later is not a guaranteed recovery: it can change the execution outcome.
  Neither withholding data nor forcing a revert is a valid substitute for proving
  the original transition. Stop and investigate the block; recovery may require
  chain-specific rollback/re-execution if permitted by its finality and settlement
  state. Do not change receipts, disable checks or bypass execution permission to
  make it pass.

Prevent this failure by keeping committed-data reads within account transactions,
preserving that boundary through indirect calls and upgrades, and retaining the
authenticated historical data needed by both execution and proving nodes.

`os_hints_config.use_committed_data` defaults to `false`. Set it to `true` to
permit reads during both Blockifier replay and Cairo OS proving. A disabled read
rejects the account transaction/proof; it never falls back to ordinary contract
execution or becomes an accepted account-transaction revert. The special address
is permanently reserved, including for deployment, regardless of this flag.

This flag is an execution permission, not a consensus activation policy. It does
not change the OS configuration hash or the public output format. Activation-height
and reader-list inputs have been removed and are rejected in structured input.
Each calling contract must still validate the root's authorization and freshness.
The aggregator needs no committed-data flag.

SNOS pins sequencer `0149bf12a9184edd07241c0d3465a85a1a036446`, which supplies the
regenerated OS, virtual-OS and aggregator programs and hashes. The same binary can
process ordinary blocks with reads disabled and committed-data blocks with reads
enabled, subject to the protocol versions supported by that dependency. This does
not make it interchangeable with historical OS program hashes. Settlement/prover
configuration must accept the pinned programs; clients using virtual-OS proof facts
must separately update their allowed program hashes. Existing publisher-bound
snapshots require regenerated roots and witnesses, and the new roots must be
published in authenticated contract state. They cannot replace old roots during
historical replay; retain the matching older proving stack for those blocks.

`generate_pie(PieGenerationInput)` accepts witnesses directly in
`os_hints_config.committed_data_witnesses`. The binary accepts the complete typed
request as JSON with `generate-pie --input-stdin`; it rejects competing CLI options.
Alternatively, set `committed_data_rpc_url` in the generation input (or
`--committed-data-rpc-url` on the CLI) to an operator-controlled Madara admin RPC.
Replay fetches only the witnesses actually read and passes the collected witnesses
to the OS in memory. Set `os_hints_config.use_committed_data` to `true`
(or `--use-committed-data` / `SNOS_USE_COMMITTED_DATA=true`). A path-based witness
file remains a standalone CLI convenience, not a requirement for orchestration. Both `generate-pie`
and `rpc-replay` accept `--use-committed-data`, `--committed-data-rpc-url` and
`--committed-data-witnesses-path`. Witness objects contain only `root`, `index`,
`value` and the 19-element `siblings` array. RPC requests use
`madara_getCommittedDataWitness(root, index)`. Repeated reads share one cached
witness per `(root, index)`; duplicate tuples in supplied witness arrays are rejected.

Version one supports up to 524,288 indexed values per root and 65,536 distinct
witnesses per generation request. Structured JSON is limited to 128 MiB; each
witness response is limited to 8 KiB and a 30-second request timeout. Oversized,
malformed, duplicate or mismatching witnesses fail. Missing data also fails replay;
it is never replaced with zero or converted into a provable arbitrary revert.
Configure batch sizes and retention accordingly. Every proving node must retain or
be able to retrieve historical datasets for the roots it replays. Committing a root
does not make the dataset available or its underlying real-world values truthful.

The current version conservatively charges 1,000,000 additional Sierra gas per
successful read and accounts for OS work in Blockifier. Recalibrating these protocol
constants requires coordinated execution/prover changes and new validation.

Adapters must validate both authorized/fresh roots and valid indices before
calling this primitive. They must not expose arbitrary root/index forwarding. Publish
roots only after their complete datasets are durably imported and replicated. Otherwise
an attacker can deliberately trigger an availability failure after expensive execution,
causing an uncharged transaction rejection. Adapter code and upgrade/publication authority
are therefore part of the appchain's operational security boundary. Membership verification
still runs in Cairo for every successful read; the proof does not trust host-returned values.
