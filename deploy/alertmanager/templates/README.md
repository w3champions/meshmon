# Alertmanager Notification Templates

This directory holds the notification templates baked into the
meshmon-alertmanager image. `discord.tmpl` is required by the bundled
`alertmanager.yml`; see below for how to customize it safely.

---

## When to add a custom template

Add a template here when you need one of the following:

- A richer Discord embed with color-coded severity bands.
- A condensed summary that merges grouped alerts into a single message.
- A different notification format for a Slack or PagerDuty receiver added
  alongside the default Discord routes.

---

## Bundled template

`discord.tmpl` ships in the image at `/etc/alertmanager/templates/discord.tmpl`
and is referenced by the bundled `alertmanager.yml`. It defines
`discord.meshmon.title` and `discord.meshmon.message`, rendering one batched
message per (alertname, severity) group: a `[FIRING:N] alertname (severity)`
title and a `• source → target (protocol): description` line per alert (first
15 firing / 5 resolved, descriptions on firing lines only, then an "(N of M shown)" note, to stay inside
Discord's 4096-char limit).

---

## Customizing

Do NOT add a second file that defines the same `discord.meshmon.*` names:
Alertmanager loads templates in alphabetical order and the bundled
`discord.tmpl` wins, so the duplicate is silently ignored. Instead either:

1. Copy and edit `discord.tmpl`, then mount your version over
   `/etc/alertmanager/templates/discord.tmpl`; or
2. Add a new `*.tmpl` file that defines DIFFERENT names (e.g.
   `discord.custom.title`) and reference those in your receivers:

   ```yaml
   discord_configs:
     - webhook_url_file: /run/secrets/discord_webhook
       title: '{{ template "discord.custom.title" . }}'
       message: '{{ template "discord.custom.message" . }}'
   ```

WARNING: do not mount a directory over `/etc/alertmanager/templates` unless it
contains `discord.tmpl`. It hides the bundled file, notifications then fail at
send time, and `amtool check-config` still reports SUCCESS.

`alertmanager.yml` must keep `templates: [/etc/alertmanager/templates/*.tmpl]`.

---

## Template helper functions

Alertmanager exposes the standard Go `text/template` functions plus these
built-ins:

| Function          | Description                                              |
|-------------------|----------------------------------------------------------|
| `toUpper`         | Uppercase a string                                       |
| `toLower`         | Lowercase a string                                       |
| `title`           | Title-case a string                                      |
| `since`           | Human-readable duration since a `time.Time`              |
| `humanize`        | SI-suffix a float (e.g. `1.2k`)                          |
| `humanizeDuration`| Human-readable duration from a float in seconds          |
| `safeHtml`        | Mark string safe for HTML embedding                      |
| `match`           | Regex match check                                        |

Full reference: https://prometheus.io/docs/alerting/latest/notifications/

---
