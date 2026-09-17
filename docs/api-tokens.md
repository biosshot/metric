# API tokens and permissions

Metric personal API tokens are intended for CLI tools, CI jobs, and HTTP API automation.
Each token carries an explicit list of scopes. The effective permissions of a token are the
intersection of its scopes and the current permissions of the user who created it, so a token
cannot retain access that its owner no longer has.

Create and revoke tokens from **Settings → API tokens**. Metric shows the token secret only once.
Store it as a secret in your CI system or local credential store rather than committing it to a
repository.

## Presets and custom permissions

The token form provides presets for common workflows and a **Custom / Advanced** mode for
fine-grained access. Custom mode only shows scopes that the current user is allowed to grant.

The **Sentry CLI uploads** preset grants:

```text
debug_file:read
debug_file:write
artifact:read
artifact:write
```

This covers the debug-file and artifact-bundle upload paths used by supported `sentry-cli` and
Sentry build-tool integrations.

Organization owner and organization deletion permissions are intentionally not available as
personal-token checkboxes in the web UI.

## Scope reference

| Scope | Allows |
| --- | --- |
| `event:read` | Read stored events and event details. |
| `issue:read` | Read issues, issue details, and issue activity. |
| `issue:write` | Update issue state and other mutable issue data. |
| `project:read` | Read project metadata and project-scoped configuration. |
| `project:admin` | Manage project settings and administrative project operations. |
| `debug_file:read` | List and inspect uploaded debug information files. |
| `debug_file:write` | Upload and assemble debug information files. |
| `debug_file:delete` | Delete uploaded debug information files. |
| `artifact:read` | Read uploaded artifact and source-map metadata. |
| `artifact:write` | Upload and assemble source maps and artifact bundles. |
| `artifact:delete` | Delete uploaded artifacts and artifact bundles. |
| `release:read` | Read releases, deploys, and release metadata. |
| `release:write` | Create and update releases and deploys. |
| `incident:export` | Export Incident Capsules for authorized issue and event data. |
| `organization:admin` | Perform organization administration allowed to organization admins. |

Some operations require more than one permission. For example, exporting an Incident Capsule also
requires access to the underlying issue and event data. Grant the smallest set of scopes required
by the integration and create separate tokens for unrelated automation.

## Choosing permissions

For read-only automation, prefer the read scopes for the resources the integration needs. For
uploads, use the dedicated write scopes instead of a broad administrative token. Only add delete
or admin scopes when the automation actually performs those operations.

If an integration returns `403 Forbidden`, check the endpoint or integration documentation for the
required scope and compare it with the scopes shown for the token in Metric. Creating a new token is
required when the scope set needs to change; existing token secrets are not expanded in place.
