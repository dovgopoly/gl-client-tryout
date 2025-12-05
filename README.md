# Greenlight Client Test

## Setup

```bash
git submodule update --init
docker compose up gltestserver
```

Wait for the server to start. You should see logs similar to:
```
Writing .env file to /repo/.gltestserver/.env
gltestserver-1  | {
gltestserver-1  | │   'scheduler_grpc_uri': 'https://localhost:39095',
gltestserver-1  | │   'grpc_web_proxy_uri': 'http://localhost:42687',
gltestserver-1  | │   'bitcoind_rpc_uri': 'http://rpcuser:rpcpass@localhost:40233',
gltestserver-1  | │   'cert_path': '/repo/.gltestserver/gl-testserver/certs',
gltestserver-1  | │   'ca_crt_path': '/repo/.gltestserver/gl-testserver/certs/ca.crt',
gltestserver-1  | │   'nobody_crt_path': '/repo/.gltestserver/gl-testserver/certs/users/nobody.crt',
gltestserver-1  | │   'nobody_key_path': '/repo/.gltestserver/gl-testserver/certs/users/nobody-key.pem'
gltestserver-1  | }
```

The run in another terminal:

```bash
docker compose up rust-test
```
