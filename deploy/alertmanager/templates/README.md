# Alertmanager Notification Templates

Alertmanager ships with a built-in Discord receiver that produces a
plain-text embed with the alert name, labels, and annotations.  For most
meshmon deployments this default output is sufficient.

This directory exists so operators can drop in custom `*.tmpl` files and
reference them from `alertmanager.yml` without modifying the bundled
configuration.

---

## When to add a custom template

Add a template here when you need one of the following:

- A richer Discord embed with color-coded severity bands.
- A condensed summary that merges grouped alerts into a single message.
- A different notification format for a Slack or PagerDuty receiver added
  alongside the default Discord routes.

---

## Adding a template

1. Create a file with a `.tmpl` extension in this directory, for example
   `discord-rich.tmpl`.

2. Define a named template block inside it:

   ```
   {{ define "discord.meshmon.title" -}}
   [{{ .Status | toUpper }}] {{ .CommonLabels.alertname }}
   severity={{ .CommonLabels.severity }}  category={{ .CommonLabels.category }}
   {{- end }}

   {{ define "discord.meshmon.message" -}}
   {{- range .Alerts }}
   source={{ .Labels.source }}  target={{ .Labels.target }}
   started={{ .StartsAt | since }}
   {{ .Annotations.description }}
   {{- end }}
   {{- end }}
   ```

3. Mount the file into the Alertmanager container.  In
   `deploy/docker-compose.yml`, under the `meshmon-alertmanager` service:

   ```yaml
   volumes:
     - ./alertmanager/alertmanager.yml:/etc/alertmanager/alertmanager.yml:ro
     - ./alertmanager/templates:/etc/alertmanager/templates:ro
   ```

4. Register the templates directory in `alertmanager.yml`:

   ```yaml
   templates:
     - /etc/alertmanager/templates/*.tmpl
   ```

5. Reference the template names in the receiver definition:

   ```yaml
   receivers:
     - name: discord-default
       discord_configs:
         - webhook_url_file: /run/secrets/discord_webhook
           title: '{{ template "discord.meshmon.title" . }}'
           message: '{{ template "discord.meshmon.message" . }}'
   ```

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

## Default (no template files)

If this directory remains empty (only this README), Alertmanager uses its
built-in default formatting.  The bundled `alertmanager.yml` does not
reference any custom template names, so no configuration changes are required
to deploy without custom templates.
