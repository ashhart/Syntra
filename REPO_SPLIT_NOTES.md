# Repo Split Notes

## What this repo is

**Lycan Language** — the AI-native programming language, compiler, graph format, CLI runtime, capabilities, and examples.

## What moved where

| Content | Location |
|---|---|
| Language source (src/*.rs) | This repo |
| Compiler, interpreter, graph executor | This repo |
| Capability registry | This repo |
| Evolution engine | This repo |
| CLI (lycan compile/run/decide/feedback/evolve) | This repo |
| Tests | This repo |
| Language examples | This repo (curated into categories) |
| Dockerfile, docker-compose.yml | Lycan-Neural-Engine |
| Server (src/server.rs) | Both (needed to build; will become engine-only) |
| Store (src/store.rs) | Both (needed to build; will become engine-only) |
| Admin console HTML | Lycan-Neural-Engine |
| API demos (demo-appliance, demo-docker-quickstart) | Lycan-Neural-Engine |
| Deployment docs | Lycan-Neural-Engine |

## TODO

- [ ] Split server.rs and store.rs out of language repo (make them engine-only)
- [ ] Make Neural Engine depend on Lycan as a library crate
- [ ] Publish Lycan as a crate on crates.io
- [ ] Separate CLI binary from library
- [ ] Add language specification docs
- [ ] Add syntax highlighting / editor support
