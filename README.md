# Greenlight Client Test

## Setup

```bash
git submodule update --init
docker compose up gltestserver
```

Once you see the server is ready in logs, run in another terminal:

```bash
docker compose up rust-test
```
