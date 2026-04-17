# Alertmanager Webhook Secrets

This directory holds the Discord webhook URL files consumed by Alertmanager
via the `webhook_url_file` directive.  Each file contains exactly one URL.

These files are excluded from version control (see `deploy/.gitignore`).
Only this README and `.gitkeep` are tracked.

---

## Required files

| Filename                   | Receiver          | Description                                         |
|----------------------------|-------------------|-----------------------------------------------------|
| `discord_webhook`          | `discord-default` | Default channel — receives all routed alerts        |
| `discord_webhook_critical` | `discord-critical`| Critical-severity channel (optional; see below)     |
| `discord_webhook_info`     | `discord-info`    | Informational / warning channel (optional)          |

### File format

Each file must contain a single line with no trailing newline:

```
https://discord.com/api/webhooks/<id>/<token>
```

Example using `printf` to avoid a trailing newline:

```bash
printf 'https://discord.com/api/webhooks/1234567890/abcdefghij' \
  > deploy/alertmanager/secrets/discord_webhook
```

Verify the file has no newline:

```bash
xxd deploy/alertmanager/secrets/discord_webhook | tail -1
# The last byte should NOT be 0a (newline).
```

---

## Single-channel setup

If you route all alerts to a single Discord channel, create the default file
and symlink the other two to it:

```bash
# Create the canonical webhook file
printf 'https://discord.com/api/webhooks/...' \
  > deploy/alertmanager/secrets/discord_webhook

# Symlink the optional files (Alertmanager follows symlinks)
ln -s discord_webhook deploy/alertmanager/secrets/discord_webhook_critical
ln -s discord_webhook deploy/alertmanager/secrets/discord_webhook_info
```

Alternatively, copy the file if your container runtime does not resolve
symlinks across bind mounts:

```bash
cp deploy/alertmanager/secrets/discord_webhook \
   deploy/alertmanager/secrets/discord_webhook_critical
cp deploy/alertmanager/secrets/discord_webhook \
   deploy/alertmanager/secrets/discord_webhook_info
```

---

## Webhook rotation

1. Obtain the new Discord webhook URL from the Discord channel settings.
2. Replace the file contents:
   ```bash
   printf 'https://discord.com/api/webhooks/<new-id>/<new-token>' \
     > deploy/alertmanager/secrets/discord_webhook
   ```
3. Signal Alertmanager to reload its configuration:
   ```bash
   # If running via docker compose:
   docker compose kill -s HUP meshmon-alertmanager

   # Or send SIGHUP directly to the process:
   kill -HUP "$(pidof alertmanager)"
   ```
   Alertmanager re-reads `webhook_url_file` paths on SIGHUP without
   restarting the process.

4. Send a test alert to confirm the new webhook is active.  This dispatches
   a real alert through the running Alertmanager so you can eyeball the
   Discord channel for delivery:
   ```bash
   source deploy/versions.env
   docker run --rm --network host \
     --entrypoint /bin/amtool \
     prom/alertmanager:${ALERTMANAGER_TAG} \
     alert add \
     --alertmanager.url=http://127.0.0.1:9093 \
     alertname=WebhookRotationTest severity=warning category=loss \
     source=manual target=manual protocol=icmp
   ```
   The test alert auto-resolves after Alertmanager's `resolve_timeout`
   (default 5 minutes).  To resolve it manually, re-run `amtool alert add`
   with a past `--end` timestamp (e.g. `--end=$(date -u +%Y-%m-%dT%H:%M:%SZ)`).

---

## Permissions

These files contain credentials.  Restrict read access to the process that
runs Alertmanager:

```bash
chmod 600 deploy/alertmanager/secrets/discord_webhook*
```

If running as a non-root user in Docker, ensure the container user has read
access, or mount the directory with appropriate ownership.
