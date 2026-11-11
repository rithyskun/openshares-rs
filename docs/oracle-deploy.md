# Deploy — Oracle Cloud Always Free (Ampere A1) + Cloudflare Tunnel

The single `Dockerfile` image (axum `:8080` + Next standalone `:3000` under
`openshares-entrypoint`) runs as-is on an Ampere A1 ARM64 VM. Cloudflare
Tunnel provides HTTPS ingress with **no inbound ports** — the VM dials
out; only SSH stays open. This is the permanently-free deploy shape.

## One-time VM prep (Ubuntu)

```bash
sudo apt-get update && sudo apt-get install -y docker.io docker-compose-v2
sudo usermod -aG docker "$USER"   # re-login after this
```

OCI security list / NSG ingress: **TCP 22 only**. The tunnel needs
outbound 443/7844 only — no public ports to open.

## Get the image onto the VM

### Option A — build on the VM (simplest, no git creds needed)

```bash
# from the mac — rsync the source, build where it runs
rsync -az --delete \
  --exclude target --exclude node_modules --exclude .next --exclude .git \
  ./ oracle@<vm-ip>:~/openshares-rs/

ssh oracle@<vm-ip> 'cd ~/openshares-rs && docker compose build'
# ~15-25 min first build on 4 Ampere cores; layer-cached after
```

### Option B — build on the Mac, ship the image (faster rebuilds)

The Mac is ARM64, Ampere A1 is ARM64 — no cross-build needed.

```bash
docker build -t openshares:arm64 .
docker save openshares:arm64 | ssh -C oracle@<vm-ip> docker load
```

## Cloudflare Tunnel (one time)

1. Zero Trust dashboard → **Networks → Tunnels → Add tunnel → cloudflared**.
2. Copy the token → `TUNNEL_TOKEN` in the VM `.env`.
3. Public hostname `openshares.<your-domain>` → service
   **`http://server:8080`** (`server` = compose service name, resolved on
   the shared docker network). The DNS record is created automatically
   on your Cloudflare zone.

## `.env` on the VM

```
APP_URL=https://openshares.<your-domain>
TURSO_DB_URL=libsql://<db>.turso.io
TURSO_AUTH_TOKEN=<jwt>
MEDIA_CACHE_BACKEND=s3
R2_ACCOUNT_ID=<acct>
R2_ACCESS_KEY_ID=<key>
R2_SECRET_ACCESS_KEY=<secret>
R2_BUCKET=openshares-cache
BOT_TOKEN=<tg-bot-token>
WEBHOOK_SECRET=<secret>
JWT_PRIVATE_KEY=...
JWT_PUBLIC_KEY=...
REQUEST_SIGNING_SECRET=...
RSA_PRIVATE_KEY=...
RSA_PUBLIC_KEY=...
ALLOWED_TELEGRAM_IDS=...
TUNNEL_TOKEN=<from dashboard>
```

## Run

```bash
cd ~/openshares-rs
docker compose --profile tunnel up -d
docker compose logs -f server      # sanity: axum :8080 + next :3000
curl https://openshares.<domain>/healthz   # → 200 through the tunnel
```

## Telegram webhook (bot without a poller)

`/api/bot/webhook` is served by the api container — register once, no
bot process needed on the server (long-poll is dev-only and must never
run alongside an active webhook):

```bash
curl "https://api.telegram.org/bot<BOT_TOKEN>/setWebhook" \
  -d url="https://openshares.<domain>/api/bot/webhook" \
  -d secret_token="<WEBHOOK_SECRET>" \
  -d 'allowed_updates=["message","channel_post","callback_query"]'
```

## Updates

```bash
rsync -az --delete \
  --exclude target --exclude node_modules --exclude .next --exclude .git \
  ./ oracle@<vm-ip>:~/openshares-rs/
ssh oracle@<vm-ip> \
  'cd ~/openshares-rs && docker compose build && docker compose --profile tunnel up -d'
```

(or `docker save | docker load` per Option B — then only `up -d` is needed.)

## Caveats

- **Idle reclaim**: OCI can stop Always-Free VMs that sit under its
  utilization threshold for ~7 days. The running container + tunnel is
  usually enough activity; an hourly
  `curl -fs localhost:8080/healthz >/dev/null` cron on the VM is cheap
  insurance.
- **`openshares-data` volume** holds `data/` (fs-cache fallback). With
  `MEDIA_CACHE_BACKEND=s3` and Turso remote, losing the volume only
  loses the warm cache — keep a backup policy for it anyway.
- 24 GB RAM leaves plenty for cargo builds + runtime; the `lite`-class
  shapes elsewhere would be tighter.
