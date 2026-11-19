# Reconciliation benches

Standalone harness for choosing between MSC4521 strategies without building
the server or linking RocksDB.

It pins the same `rezzy-recon` revision as the workspace and compares:

- GF(2^64) fixed `SortedPopulation` vs GF(2^256) fixed and incremental
  populations
- the cost of building a root sketch over the whole population
- adaptive depth-0-then-split rounds and upload bytes
- the optional strata estimator's time, estimate, and value
- modeled latency using configurable RTT and bandwidth

## Run

```sh
cargo run --release --manifest-path benches/Cargo.toml

cargo run --release --manifest-path benches/Cargo.toml -- \
    --population 1000000 --deltas 0,32,64,128,512 \
    --rtt-ms 50 --bandwidth-mbps 20
```

Options:

- `--population N` base event count (default 100000)
- `--deltas a,b,c` symmetric-difference sizes (default 0,1,4,16,32,64,128,512)
- `--rtt-ms F` round-trip time for the latency model (default 50)
- `--bandwidth-mbps F` link bandwidth (default 20)
- `--one-sided` make the delta one-sided instead of split across both peers

## Reading it

The first table answers "what does one root sketch cost?". Compare the fixed
population with the incremental one:

- fixed build is O(population) every round;
- incremental build is O(population) once per populated state, then root
  queries XOR cached leaves;
- the cache depth is 7 (128 leaves) in this revision.

The incremental build is only amortized if the state survives more than one
round or exchange. If the homeserver rebuilds the population for every
request, add `incremental cache build` to the first round; for a one-round
small diff that can erase the win.

The second table answers "when is strata worth 2 KiB?". `r64`/`r256` are the
adaptive rounds, `k64`/`k256` the requester upload bytes, `est` the strata
estimate, and the two latency columns use the measured incremental CPU.

The model assumes:

- adaptive sends one depth-0 sketch, then splits only overflowing buckets;
- strata costs a digest/strata preflight plus one provisioned diff round;
- both paths pay the same O(population) snapshot build, which is excluded;
- the incremental cache build is also excluded, so the latency columns assume
  a state that is reused across rounds (or across exchanges);
- responses cost 44 B per difference item.

Under those assumptions strata cannot win when adaptive needs one or two
rounds, because the strata preflight itself costs a round. It only pays when
splitting needs three or more rounds.

These are synthetic uniform digests. Clustered or adversarial populations can
change the round counts; use multiple distributions before making a final
choice.
