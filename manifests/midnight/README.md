# Default Midnight ingestion manifests

Directory-level `hierarchy.yaml` manifests for the default Midnight corpus
(`midnight-manual`). One file per source repo; **no individual-file leaves** —
each manifest pins whole directories via `path:` + glob `include`/`exclude`.
Owner and trust live in each manifest's `root.provenance` (which inherits to all
descendants). See the design spec
`docs/superpowers/specs/2026-06-01-midnight-ingestion-manifests-design.md`.

## Provenance / trust model

`attribution` reflects real authorship (`foundation` / `partner` / `third_party`
/ `community`); `verified: true` is set only for Foundation (first-party,
canonical) content. Each manifest also carries a `tags: [trust:<level>]` label
for humans and filters; `trust:low` entries carry a `verification_notes`
explaining the rating. Confidence scoring (D24) is driven by `attribution` +
`verified`.

## Using a manifest

Git URL / branch / slug are **not** stored in the manifests. Register each source
first, then ingest against a fresh checkout (`ingest run` uses `--source-root`):

```bash
git clone --depth=1 -b main https://github.com/midnightntwrk/midnight-docs /tmp/clones/midnight-docs
mnm sources create --slug midnight-docs --kind docs_site \
    --origin-url https://github.com/midnightntwrk/midnight-docs
mnm ingest run manifests/midnight/midnight-docs.yaml \
    --source-slug midnight-docs --source-root /tmp/clones/midnight-docs
```

`--kind` is one of `docs_site | code_repo | standalone | mixed`. (`ingest run`
also auto-creates the source on 404 with `--yes`, but explicit `sources create`
is preferred so `kind`/`origin_url` are set.)

## Index (44 sources)

| slug | repo | branch | kind | owner | attribution | verified | trust |
|------|------|--------|------|-------|-------------|----------|-------|
| midnight-ledger | midnightntwrk/midnight-ledger | main | mixed | Foundation | foundation | true | high |
| midnight-node | midnightntwrk/midnight-node | main | mixed | Foundation | foundation | true | high |
| midnight-indexer | midnightntwrk/midnight-indexer | main | mixed | Foundation | foundation | true | high |
| midnight-js | midnightntwrk/midnight-js | main | code_repo | Foundation | foundation | true | high |
| midnight-wallet | midnightntwrk/midnight-wallet | main | code_repo | Foundation | foundation | true | high |
| midnight-sdk | midnightntwrk/midnight-sdk | main | code_repo | Foundation | foundation | true | high |
| midnight-dapp-connector-api | midnightntwrk/midnight-dapp-connector-api | main | code_repo | Foundation | foundation | true | high |
| midnight-local-dev | midnightntwrk/midnight-local-dev | main | code_repo | Foundation | foundation | true | high |
| midnight-docs | midnightntwrk/midnight-docs | main | docs_site | Foundation | foundation | true | high |
| midnight-improvement-proposals | midnightntwrk/midnight-improvement-proposals | main | docs_site | Foundation | foundation | true | high |
| midnight-architecture | midnightntwrk/midnight-architecture | main | docs_site | Foundation | foundation | true | high |
| midnight-awesome-dapps | midnightntwrk/midnight-awesome-dapps | main | docs_site | Foundation | foundation | true | high |
| example-counter | midnightntwrk/example-counter | main | mixed | Foundation | foundation | true | high |
| example-bboard | midnightntwrk/example-bboard | main | mixed | Foundation | foundation | true | high |
| example-battleship | midnightntwrk/example-battleship | main | mixed | Foundation | foundation | true | high |
| example-hello-world | midnightntwrk/example-hello-world | main | mixed | Foundation | foundation | true | high |
| example-zkloan | midnightntwrk/example-zkloan | main | mixed | Foundation | foundation | true | high |
| example-kitties | midnightntwrk/example-kitties | main | mixed | Foundation | foundation | true | high |
| example-private-party | midnightntwrk/example-private-party | main | mixed | Foundation | foundation | true | high |
| example-nft-contracts | midnightntwrk/example-nft-contracts | main | mixed | Foundation | foundation | true | high |
| midnight-wallet-dapp | midnightntwrk/midnight-wallet-dapp | main | mixed | Foundation | foundation | true | high |
| midnight-leaderboard | midnightntwrk/midnight-leaderboard | main | mixed | Foundation | foundation | true | high |
| midnight-tip-jar | midnightntwrk/midnight-tip-jar | main | mixed | Foundation | foundation | true | high |
| midnight-dust-generator | midnightntwrk/midnight-dust-generator | main | mixed | Foundation | foundation | true | high |
| compact | midnightntwrk/compact | main | docs_site | Foundation | foundation | true | high |
| create-mn-app | midnightntwrk/create-mn-app | main | mixed | Foundation | foundation | true | high |
| setup-compact-action | midnightntwrk/setup-compact-action | main | docs_site | Foundation | foundation | true | high |
| midnight-node-docker | midnightntwrk/midnight-node-docker | main | mixed | Foundation | foundation | true | high |
| contributor-hub | midnightntwrk/contributor-hub | main | docs_site | Foundation | foundation | true | high |
| servicedesk | midnightntwrk/servicedesk | main | docs_site | Foundation | foundation | true | high |
| midnight-reserve-contracts | midnightntwrk/midnight-reserve-contracts | main | mixed | Foundation | foundation | true | high |
| night-token-distribution | midnightntwrk/night-token-distribution | main | mixed | Foundation | foundation | true | high |
| openzeppelin-compact-contracts | OpenZeppelin/compact-contracts | main | mixed | Partner | partner | false | high |
| openzeppelin-compact-tools | OpenZeppelin/compact-tools | main | code_repo | Partner | partner | false | high |
| openzeppelin-midnight-apps | OpenZeppelin/midnight-apps | main | mixed | Partner | partner | false | high |
| eddalabs-midnight-starter-template | eddalabs/midnight-starter-template | main | mixed | Partner | partner | false | medium |
| bricktowers-midnight-rwa | bricktowers/midnight-rwa | main | mixed | Partner | partner | false | medium |
| bricktowers-midnight-identity | bricktowers/midnight-identity | main | mixed | Partner | partner | false | medium |
| bricktowers-midnight-seabattle | bricktowers/midnight-seabattle | main | mixed | Partner | partner | false | medium |
| midnames-core | midnames/core | main | mixed | Partner | partner | false | medium |
| joacolinares-kyc-midnight | joacolinares/kyc-midnight | main | mixed | Hackathon Winner | third_party | false | medium |
| olanetsoft-learn-compact | Olanetsoft/learn-compact | main | docs_site | Other 3rd party | community | false | medium |
| olanetsoft-compact-by-example | Olanetsoft/compact-by-example | main | docs_site | Other 3rd party | community | false | medium |
| adavault-midnight-skill | ADAvault/midnight-skill | main | docs_site | Other 3rd party | community | false | low |
