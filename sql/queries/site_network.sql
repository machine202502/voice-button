-- name: read
SELECT port, enabled, firewall_rule, firewall_created, upnp_router,
    upnp_external_port, upnp_internal_ip, upnp_added, error, updated
FROM site_network WHERE id = 1;

-- name: write
INSERT INTO site_network(id, port, enabled, firewall_rule, firewall_created,
    upnp_router, upnp_external_port, upnp_internal_ip, upnp_added, error, updated)
VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(id) DO UPDATE SET port = excluded.port, enabled = excluded.enabled,
    firewall_rule = excluded.firewall_rule, firewall_created = excluded.firewall_created,
    upnp_router = excluded.upnp_router, upnp_external_port = excluded.upnp_external_port,
    upnp_internal_ip = excluded.upnp_internal_ip, upnp_added = excluded.upnp_added,
    error = excluded.error, updated = excluded.updated;
