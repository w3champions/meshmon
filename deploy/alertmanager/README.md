# Alertmanager config

`alertmanager.yml` and the `templates/` directory are baked into the
`ghcr.io/w3champions/meshmon-alertmanager` image. Discord webhook URLs
are injected at container start via docker-compose's `secrets:` stanza
(environment-sourced) and referenced by `webhook_url_file:` inside the
baked config — nothing touches the host filesystem.

Downstream deployments override the webhook plumbing by either building
a thin overlay image that replaces `/etc/alertmanager/alertmanager.yml`,
or by remounting the file at runtime from the host.
