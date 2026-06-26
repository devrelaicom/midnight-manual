# Default Midnight ingestion manifests

Directory-level `hierarchy.yaml` manifests for the default Midnight corpus
(`midnight-manual`). One file per source repo; **no individual-file leaves** —
each manifest pins whole directories via `path:` + glob `include`/`exclude`.
Owner and trust live in each manifest's `root.provenance` (which inherits to all
descendants).

### Layout

Most manifests live at the top level of `manifests/midnight/`. Community-
contributed third-party sources live in the **`community/`** subfolder (further
category subfolders may be added later). `sources.tsv` and the ingest tooling
resolve a slug to `<slug>.yaml` at the top level first, then fall back to a
one-level subdirectory (`community/<slug>.yaml`). Slugs are unique across
folders.

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

## Index (72 sources)

> The top-level table below predates the `community/` reorg and is **not**
> CI-enforced against `sources.tsv` on the `kind`/owner columns — treat
> `sources.tsv` as authoritative. Community sources are listed in their own
> section after it.

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
| bricktowers-midnight-rwa | bricktowers/midnight-rwa | main | mixed | Partner | partner | false | medium |
| bricktowers-midnight-identity | bricktowers/midnight-identity | main | mixed | Partner | partner | false | medium |
| bricktowers-midnight-seabattle | bricktowers/midnight-seabattle | main | mixed | Partner | partner | false | medium |
| midnames-core | midnames/core | main | mixed | Partner | partner | false | medium |
| joacolinares-kyc-midnight | joacolinares/kyc-midnight | main | mixed | Hackathon Winner | third_party | false | medium |
## Community sources (`manifests/midnight/community/`)

Community-contributed third-party repos. All are `attribution: community`,
`verified: false`, with a `verification_notes` explaining the rating. The 28
repos added 2026-06-26 index only `.compact` / `.ts` / `.tsx` / `.js` / `.jsx` /
`.mjs` / `.cjs` / `.rs` / `.md` / `.mdx` (boilerplate Markdown such as
`SECURITY.md` excluded). The four pre-existing entries (eddalabs, Olanetsoft ×2,
ADAvault) retain their original per-node scoping.

| slug | repo | branch | kind | owner | attribution | verified | trust |
|------|------|--------|------|-------|-------------|----------|-------|
| eddalabs-midnight-starter-template | eddalabs/midnight-starter-template | main | mixed | Community | community | false | low |
| olanetsoft-learn-compact | Olanetsoft/learn-compact | main | docs_site | Community | community | false | medium |
| olanetsoft-compact-by-example | Olanetsoft/compact-by-example | main | docs_site | Community | community | false | medium |
| adavault-midnight-skill | ADAvault/midnight-skill | main | docs_site | Community | community | false | low |
| 0xfdbu-midnight-unshielded-token | 0xfdbu/midnight-unshielded-token | main | code_repo | Community | community | false | low |
| 0xfdbu-midnight-dapp-connect | 0xfdbu/midnight-dapp-connect | main | code_repo | Community | community | false | low |
| 0xfdbu-midnight-shielded-token | 0xfdbu/midnight-shielded-token | main | code_repo | Community | community | false | low |
| 0xfdbu-midnight-attestation-dapp | 0xfdbu/midnight-attestation-dapp | main | code_repo | Community | community | false | low |
| rambo-lc-midnight-statrter-pack | RAMBO-LC/Midnight-statrter-pack | main | docs_site | Community | community | false | low |
| rambo-lc-mn-voting-dapp | RAMBO-LC/MN-Voting-Dapp | main | code_repo | Community | community | false | low |
| paranormal39-agilitycore | paranormal39/AgilityCore | master | code_repo | Community | community | false | low |
| paranormal39-midnightunityconnector | paranormal39/MidnightUnityConnector | main | code_repo | Community | community | false | low |
| paranormal39-midnight-example-dao | paranormal39/midnight-example-dao | main | code_repo | Community | community | false | low |
| paranormal39-laylaa | paranormal39/laylaa | main | code_repo | Community | community | false | low |
| paranormal39-votechain | paranormal39/Votechain | master | code_repo | Community | community | false | low |
| paranormal39-vaultchain | paranormal39/vaultchain | master | code_repo | Community | community | false | low |
| paranormal39-agilitytools | paranormal39/AgilityTools | main | code_repo | Community | community | false | low |
| spycrypto-autodiscovery | SpyCrypto/AutoDiscovery | main | code_repo | Community | community | false | low |
| spycrypto-nightforce-vault | SpyCrypto/nightforce-vault | main | code_repo | Community | community | false | low |
| spycrypto-nightforce-intelligence | SpyCrypto/nightforce-intelligence | main | code_repo | Community | community | false | low |
| spycrypto-midnight-juror-zer0 | SpyCrypto/midnight-juror-zer0 | main | code_repo | Community | community | false | low |
| eddalabs-midnight-contracts | eddalabs/midnight-contracts | main | code_repo | Community | community | false | low |
| eddalabs-certificate-sandbox | eddalabs/certificate-sandbox | main | code_repo | Community | community | false | low |
| eddalabs-bucket-defi-dapp | eddalabs/bucket-defi-dapp | main | code_repo | Community | community | false | low |
| kali-decoder-midnight-skills | Kali-Decoder/Midnight-Skills | main | docs_site | Community | community | false | low |
| dareu-foundation-team-contract | dareu-foundation-team/contract | main | code_repo | Community | community | false | low |
| nstanford5-compact-testbed | nstanford5/compact-testbed | master | code_repo | Community | community | false | low |
| nstanford5-compact-hello-world | nstanford5/compact-hello-world | master | code_repo | Community | community | false | low |
| nstanford5-example-raffle | nstanford5/example-raffle | master | code_repo | Community | community | false | low |
| nstanford5-example-battleship-simple | nstanford5/example-battleship-simple | master | code_repo | Community | community | false | low |
| nstanford5-example-private-auction-reserve | nstanford5/example-private-auction-reserve | master | code_repo | Community | community | false | low |
| nstanford5-example-private-party | nstanford5/example-private-party | main | code_repo | Community | community | false | low |
