# ADR-0050: Telegram destinations and schema generation 20

- Status: Accepted
- Date: 2026-09-04
- Implementation: Complete
- Amends: ADR-0045 and ADR-0049
- Storage effect: automatic MongoDB migration from generation 19 to 20

## Context

The existing Telegram integration stores one encrypted bot token and one numeric
chat identifier in every notification destination. Project administrators can find
private subscribers through a browser-generated pairing code and `getUpdates`, but
cannot enter a channel identifier directly, select a forum topic, or route through a
self-hosted Bot API server or reverse proxy. Delivery always uses the process-wide
`https://api.telegram.org` constant.

Telegram accepts either a numeric chat identifier or a public `@username` as a chat
selector. A bot may deliver to many chats, and one forum supergroup may contain many
topics identified by `message_thread_id`. A destination must therefore identify one
specific `(bot, chat, optional thread)` route rather than treating a bot as having a
single subscriber.

Allowing an administrator-provided Bot API base also creates an outbound-network
boundary. The API base may legitimately be HTTP for local development or an internal
Bot API deployment, while access to private addresses remains an operator decision.

## Decision

### Destination model

One notification destination continues to represent one independently selectable
delivery target. A single bot may own any number of destinations:

```text
bot credentials
  -> chat A
  -> chat B
  -> supergroup C / topic 41
  -> supergroup C / topic 73
```

The existing compact `u` field remains the canonical numeric Telegram `chat.id`,
encoded as a string so Telegram's signed identifiers remain lossless across JSON and
BSON boundaries. The existing sealed `s` field remains the bot token. Telegram
destinations gain a required compact `g` object:

```javascript
{
  k: "telegram",
  u: "-1001234567890",
  s: <sealed bot token>,
  g: {
    h: "https://api.telegram.org",
    t: 73, // optional message_thread_id
    b: 123456, // optional bot id snapshot
    u: "metric_alerts_bot", // optional bot username snapshot
    n: "Metric Alerts", // optional bot display-name snapshot
    y: "supergroup", // optional chat kind snapshot
    a: "on_call", // optional chat username snapshot
    d: "On-call" // optional chat display-name snapshot
  }
}
```

Bot credentials remain copied and independently sealed in each destination. A
separate bot-connection collection is deferred: it would substantially enlarge this
change, complicate migration of randomized ciphertext, and is not required to
support one bot with many targets. The Web groups destinations by their bot-id/API
base snapshots into a virtual list of available bots. When an administrator selects
one, the request names a source destination and the server opens and reseals its
token without ever returning plaintext to the browser. The selected API base is
inherited with that token; physically it remains stored per destination rather than
being a project-global setting. Deterministic discovered destination identity includes
project, Telegram bot id, numeric chat id and optional thread id. The existing
identity is retained for destinations without a thread.

The domain model uses a provider-specific `TelegramDestination` value. Its API base
is bounded and normalized, and its optional thread id must be positive. The ordinary
destination endpoint remains the canonical chat id to avoid changing rule and
delivery references.

### Manual configuration and username resolution

The Web accepts an API base, bot token, numeric chat id or `@username`, and an
optional message thread id. The server resolves both numeric and username inputs
through the selected Bot API `getChat` method and stores the returned numeric
`chat.id`. A username is never the durable routing authority because it can be
renamed or reassigned. Bounded bot and chat identity snapshots from `getMe`/`getChat`
are persisted only for presentation and reuse; they are never routing authority.
Private users cannot be resolved from an arbitrary `@username`: they must first send
the pairing command to the bot. Public channel and supergroup usernames remain valid
manual selectors. Provider failures are mapped to stable, actionable API codes rather
than being flattened into `invalid_request`.

A recipient can be removed from active delivery by disabling its destination. This
is a soft delete: rules and delivery history retain their references, disabled
destinations cannot send, and an administrator may restore one later. A disabled
destination may still be the credential source for adding another recipient.

The Bot API base defaults to `https://api.telegram.org`. Both `http` and `https` are
valid without a separate transport opt-in. The Web shows a non-blocking warning for
HTTP because the Bot API protocol places the bearer token in the request path.

### Automatic target discovery

Pairing is presented as `Find Chat ID automatically`, not as a second kind of bot
connection. After the administrator explicitly starts discovery, Metric displays a
random bounded pairing code in a private-chat start link and as a copyable `/start
<code>` command. The browser immediately performs bounded long polling; there is no
second `Sync` button. Discovery stops after a fixed deadline, navigation or success,
and offers an explicit retry after timeout.

For a private chat the user follows the start link. For a group or forum topic the
user adds the bot and posts the displayed command inside the intended group/topic.
Metric reads matching Bot API updates and extracts `message.chat.id` plus optional
`message.message_thread_id`. Multiple matching pairs may be returned and are
deduplicated by `(chat_id, thread_id)`, so one bot can discover many chats and many
topics within one chat. The browser carries the returned `getUpdates` offset into
the next bounded poll, advancing through an existing backlog and acknowledging only
updates already returned by Telegram.

Discovery is an optional convenience. It cannot operate when another application
owns `getUpdates` or the bot has an active webhook. Manual `getChat` configuration
remains the universal path and does not replace or mutate a webhook.

### Outbound network policy

Telegram API requests, webhook delivery, uptime checks and SMTP currently share
parts of private-address rejection but duplicate DNS resolution and pinning logic.
The common address classification and resolve-then-pin behavior moves to a server
outbound-network module. Provider modules retain their own URL-shape rules.

Telegram API bases:

- must use `http` or `https`;
- must contain a host;
- may contain a port and path prefix;
- must not contain user information, a query or a fragment;
- never follow redirects;
- resolve before connection and pin the request to a checked address;
- apply `notifications.telegram.allow_private_networks`, which defaults to `true`
  so local and self-hosted Bot API deployments work without extra setup; an operator
  may set it to `false` to reject loopback, private, link-local, multicast,
  documentation, unspecified and cloud-metadata addresses.

No log or error includes the completed request URL because it contains the bot token.

### Schema migration 19 to 20

Generation 20 is the first production use of ADR-0049's migration runner. It has
three idempotent steps:

1. install an expand validator accepting both generation-19 Telegram documents and
   generation-20 documents;
2. scan Telegram destinations by ascending `_id` in count- and byte-bounded pages,
   adding `g.h = "https://api.telegram.org"` only when `g` is absent;
3. install the strict generation-20 destination validator.

Generation 20 also permits the optional presentation snapshots above. The migration
does not invent them for existing records; they are populated on the next successful
Telegram configuration or discovery operation. There is deliberately no 20 to 21
migration: generation 20 has not shipped independently from this Telegram change.

The migration does not decrypt or rewrite tokens, change destination identifiers,
touch rule references, contact Telegram, or modify user-visible timestamps. Its
preflight accepts only the exact known source, expand or target validator so a crash
after `collMod` but before checkpoint publication remains resumable. Verification
requires the exact target validator and rejects any Telegram destination without its
new configuration before generation 20 is published.

An empty database bootstraps directly at generation 20. Existing non-Telegram
destinations are unchanged.

## Verification gate

- Domain tests cover provider/config matching, bounded API bases and positive thread
  identifiers.
- Domain and MongoDB codec tests cover bounded API bases, provider/config matching,
  positive topic identifiers and lossless custom-base/topic persistence.
- A real MongoDB migration test proves generation 19 to 20 across multiple bounded
  pages, idempotent startup, unchanged ciphertext/ids/rule references and the final
  strict validator.
- Controlled Bot API tests cover HTTP path prefixes, optional thread delivery,
  response bounds, disabled redirects and both private-address policy settings.
- Native helpers and Web tests cover numeric and username selectors, private-user
  guidance, stable no-topic identity, distinct topic identity, identity snapshots,
  saved-bot credential reuse, recipient disable/restore, one bot with multiple
  targets and automatic discovery without a second action.
- Workspace Rust checks, Clippy, tests, Web format/lint/build/tests and the real
  Docker migration test pass before completion is claimed.

## Consequences

- One bot can route independently to many chats and forum topics.
- Public usernames are convenient input but never mutable routing state.
- Local HTTP Bot API deployments work by default; operators needing a stricter
  multi-tenant boundary can disable private-network access explicitly.
- Migration 19 to 20 exercises the production runner using an additive,
  non-destructive and replay-safe transformation.
- Bot-level credential rotation across every destination remains a separate future
  concern; selecting an available bot reuses credentials but does not establish a
  new bot-owned storage aggregate.
