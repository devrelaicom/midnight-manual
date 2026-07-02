# Default Midnight ingestion manifests

Directory-level `hierarchy.yaml` manifests for the default Midnight corpus
(`midnight-manual`). One file per source repo. Each manifest describes what to
ingest via a `root` node with a `path:` directory walk and optional glob
`include`/`exclude` lists (and, for some sources, child `path:` nodes). Owner and
trust live in each manifest's `root.provenance` (which inherits to all
descendants).

These manifests are designed for the unified `FileFilter` walker, which already
auto-skips generated/vendored/build directories, hidden dot-files, lockfiles,
boilerplate Markdown (`CODE_OF_CONDUCT.md` / `CONTRIBUTING.md` / `SECURITY.md`),
and any file whose extension is not a recognised language — so manifests stay
lean and only encode repo-specific signal/noise decisions.

### Layout

Most manifests live at the top level of `manifests/midnight/`. Categorised
third-party sources live in subfolders — **`partner/`** (formal partner orgs)
and **`community/`** (community-contributed) so far; more may be added later.
`sources.tsv` and the ingest tooling resolve a slug to `<slug>.yaml` at the top
level first, then fall back to a one-level subdirectory (`partner/<slug>.yaml`,
`community/<slug>.yaml`). Slugs are unique across folders.

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

## Index (110 sources)

> This index is generated from `sources.tsv` (slug / repo / branch / kind) and
> each manifest's `root.provenance` (attribution / verified / trust). It is **not**
> CI-enforced — `sources.tsv` is authoritative; regenerate the tables if they
> drift. Partner and community sources are listed in their own sections below.

| slug | repo | branch | kind | owner | attribution | verified | trust |
|------|------|--------|------|-------|-------------|----------|-------|
| midnight-ledger | midnightntwrk/midnight-ledger | main | code_repo | Foundation | foundation | true | high |
| midnight-node | midnightntwrk/midnight-node | main | code_repo | Foundation | foundation | true | high |
| midnight-indexer | midnightntwrk/midnight-indexer | main | code_repo | Foundation | foundation | true | high |
| midnight-js | midnightntwrk/midnight-js | main | code_repo | Foundation | foundation | true | high |
| midnight-wallet | midnightntwrk/midnight-wallet | main | code_repo | Foundation | foundation | true | high |
| midnight-sdk | midnightntwrk/midnight-sdk | main | code_repo | Foundation | foundation | true | high |
| midnight-dapp-connector-api | midnightntwrk/midnight-dapp-connector-api | main | code_repo | Foundation | foundation | true | high |
| midnight-local-dev | midnightntwrk/midnight-local-dev | main | code_repo | Foundation | foundation | false | high |
| midnight-docs | midnightntwrk/midnight-docs | main | docs_site | Foundation | foundation | false | high |
| midnight-improvement-proposals | midnightntwrk/midnight-improvement-proposals | main | docs_site | Foundation | foundation | true | high |
| midnight-architecture | midnightntwrk/midnight-architecture | main | docs_site | Foundation | foundation | true | high |
| midnight-awesome-dapps | midnightntwrk/midnight-awesome-dapps | main | docs_site | Foundation | foundation | false | high |
| example-counter | midnightntwrk/example-counter | main | code_repo | Foundation | foundation | false | high |
| example-bboard | midnightntwrk/example-bboard | main | code_repo | Foundation | foundation | false | high |
| example-battleship | midnightntwrk/example-battleship | main | code_repo | Foundation | foundation | false | high |
| example-hello-world | midnightntwrk/example-hello-world | main | code_repo | Foundation | foundation | false | high |
| example-zkloan | midnightntwrk/example-zkloan | main | code_repo | Foundation | foundation | false | high |
| example-kitties | midnightntwrk/example-kitties | main | code_repo | Foundation | foundation | false | high |
| example-private-party | midnightntwrk/example-private-party | main | code_repo | Foundation | foundation | false | high |
| example-nft-contracts | midnightntwrk/example-nft-contracts | main | code_repo | Foundation | foundation | false | high |
| midnight-wallet-dapp | midnightntwrk/midnight-wallet-dapp | main | code_repo | Foundation | foundation | false | high |
| midnight-leaderboard | midnightntwrk/midnight-leaderboard | main | code_repo | Foundation | foundation | false | high |
| midnight-tip-jar | midnightntwrk/midnight-tip-jar | main | code_repo | Foundation | foundation | false | high |
| midnight-dust-generator | midnightntwrk/midnight-dust-generator | main | code_repo | Foundation | foundation | false | high |
| compact | LFDT-Minokawa/compact | main | code_repo | Foundation | foundation | true | high |
| create-mn-app | midnightntwrk/create-mn-app | main | code_repo | Foundation | foundation | true | high |
| setup-compact-action | midnightntwrk/setup-compact-action | main | code_repo | Foundation | foundation | true | high |
| midnight-node-docker | midnightntwrk/midnight-node-docker | main | code_repo | Foundation | foundation | false | high |
| contributor-hub | midnightntwrk/contributor-hub | main | docs_site | Foundation | foundation | true | high |
| servicedesk | midnightntwrk/servicedesk | main | docs_site | Foundation | foundation | false | high |
| midnight-reserve-contracts | midnightntwrk/midnight-reserve-contracts | main | code_repo | Foundation | foundation | true | high |
| night-token-distribution | midnightntwrk/night-token-distribution | main | code_repo | Foundation | foundation | true | high |
| midnight-cnight-to-dust-dapp | midnightntwrk/midnight-cnight-to-dust-dapp | main | mixed | Foundation | foundation | false | high |
| midnight-zk | midnightntwrk/midnight-zk | main | code_repo | Foundation | foundation | true | high |
| passport | midnightntwrk/passport | main | mixed | Foundation | foundation | false | high |
| joacolinares-kyc-midnight | joacolinares/kyc-midnight | ramaJoaco | code_repo | Hackathon Winner | third_party | false | medium |

## Partner sources (`manifests/midnight/partner/`)

Formal partner-org repos. All are `attribution: partner`, `verified: false`.
OpenZeppelin and input-output-hk are `trust:high`; the rest are `trust:medium`.
Manifests walk the whole repo (`path: .`, minus the walker's default skips) with
targeted excludes for repo-specific noise (vendored submodules, animation/data
blobs, duplicate trees); large multi-chain monorepos (`input-output-hk-lace`,
`openzeppelin-adapters`) and docs-only repos (`midnames-docs`,
`webisoftsoftware-1am-midnight-skill`) are scoped to their content directories.

| slug | repo | branch | kind | owner | attribution | verified | trust |
|------|------|--------|------|-------|-------------|----------|-------|
| openzeppelin-compact-contracts | OpenZeppelin/compact-contracts | main | code_repo | Partner | partner | false | high |
| openzeppelin-compact-tools | OpenZeppelin/compact-tools | main | code_repo | Partner | partner | false | high |
| openzeppelin-midnight-apps | OpenZeppelin/midnight-apps | main | code_repo | Partner | partner | false | high |
| bricktowers-midnight-rwa | bricktowers/midnight-rwa | main | code_repo | Partner | partner | false | medium |
| bricktowers-midnight-identity | bricktowers/midnight-identity | main | code_repo | Partner | partner | false | medium |
| bricktowers-midnight-seabattle | bricktowers/midnight-seabattle | main | code_repo | Partner | partner | false | medium |
| midnames-core | midnames/core | main | code_repo | Partner | partner | false | medium |
| webisoftsoftware-1am-starter-template | webisoftSoftware/1AM-starter-template | main | code_repo | Partner | partner | false | medium |
| webisoftsoftware-split-prove | webisoftSoftware/split-prove | main | code_repo | Partner | partner | false | medium |
| webisoftsoftware-1am-midnight-skill | webisoftSoftware/1AM-Midnight-Skill | main | docs_site | Partner | partner | false | medium |
| midnames-passport-circuits | midnames/passport-circuits | main | code_repo | Partner | partner | false | medium |
| midnames-docs | midnames/docs | main | docs_site | Partner | partner | false | medium |
| midnames-sdk | midnames/sdk | main | code_repo | Partner | partner | false | medium |
| midnames-vc-examples | midnames/vc-examples | main | code_repo | Partner | partner | false | medium |
| midnames-deploy-receive-test | midnames/deploy-receive-test | main | code_repo | Partner | partner | false | medium |
| midnames-did | midnames/did | main | code_repo | Partner | partner | false | medium |
| midnames-did-frontend | midnames/did-frontend | main | code_repo | Partner | partner | false | medium |
| input-output-hk-arc-mn-tui | input-output-hk/arc-mn-tui | main | code_repo | Partner | partner | false | high |
| input-output-hk-lace | input-output-hk/lace | main | code_repo | Partner | partner | false | high |
| paimastudios-midnight-game-2 | effectstream/dust-to-dust | main | code_repo | Partner | partner | false | medium |
| paimastudios-pvp-arena | effectstream/kachina-colosseum | main | code_repo | Partner | partner | false | medium |
| sundaeswap-finance-capacity-exchange | SundaeSwap-finance/capacity-exchange | main | code_repo | Partner | partner | false | medium |
| sundaeswap-finance-midnight-swaps-smart-contracts | SundaeSwap-finance/midnight-swaps-smart-contracts | main | code_repo | Partner | partner | false | medium |
| openzeppelin-adapters | OpenZeppelin/openzeppelin-adapters | main | code_repo | Partner | partner | false | high |
| no-witness-labs-midday-sdk | no-witness-labs/midday-sdk | main | code_repo | Partner | partner | false | medium |
| devrelaicom-compactp | devrelaicom/compactp | main | code_repo | Partner | partner | false | medium |
| devrelaicom-midnight-expert | devrelaicom/midnight-expert | main | docs_site | Partner | partner | false | medium |
| effectstream-block-kart-legends | effectstream/block-kart-legends | main | code_repo | Partner | partner | false | medium |
| effectstream-go-fish | effectstream/go-fish | v2 | code_repo | Partner | partner | false | medium |
| effectstream-mip-zswap-offer | effectstream/mip-zswap-offer | main | code_repo | Partner | partner | false | medium |
| effectstream-nix-nax | effectstream/nix-nax | main | code_repo | Partner | partner | false | medium |
| effectstream-safe-solver | effectstream/safe-solver | main | code_repo | Partner | partner | false | medium |
| effectstream-social-wallet-2of3 | effectstream/social-wallet-2of3 | main | code_repo | Partner | partner | false | medium |
| effectstream-werewolf-game | effectstream/werewolf-game | main | code_repo | Partner | partner | false | medium |
| effectstream-zkir-wasm-experiment | effectstream/zkir-wasm-experiment | main | code_repo | Partner | partner | false | medium |
| effectstream-zswap-presale | effectstream/zswap-presale | main | code_repo | Partner | partner | false | medium |

## Community sources (`manifests/midnight/community/`)

Community-contributed third-party repos. All are `attribution: community`,
`verified: false`, each with a `verification_notes` explaining the rating
(mostly `trust:low`; the Olanetsoft tutorial repos are `trust:medium`). Manifests
walk the whole repo (`path: .`, minus default skips) with targeted excludes for
code repos, and `**/*.md` / `**/*.mdx` or content-directory scoping for
docs/skill repos.

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
| 0xstrong-anchorzk | 0xstrong/AnchorZK | main | code_repo | Community | community | false | low |
| adamreynolds-io-compact-zkir-lint | adamreynolds-io/compact-zkir-lint | main | code_repo | Community | community | false | low |
| adamreynolds-io-gsd-wallet | adamreynolds-io/gsd-wallet | main | code_repo | Community | community | false | low |
| nel349-midnight-kicks | kuiralabs/midnight-kicks | main | code_repo | Community | community | false | low |
| nel349-kuira-midnight-ffi | nel349/kuira-midnight-ffi | main | code_repo | Community | community | false | low |
| nel349-midnight-wallet-cli | nel349/midnight-wallet-cli | main | code_repo | Community | community | false | low |
