# Tuwunel 1.8.0

June 27, 2026

### New Features & Enhancements

- **Conduit database migration** available again courtesy of @x86pup. Open Tuwunel with an existing Conduit (or foreign-lineage) database directory and it migrates in-place: rooms, original media including S3-backed objects, pending knocks, and suspended or locked users all carry over, media is attributed to its local owners, and conduit-migrated rooms become usable. Raised by @Korvox in (#41).

- **Third-party-identifier (3pid) email support** (MSC2290) arrives with outbound email, 3pid storage, the email-3pid request and management endpoints, and admin commands to manage and query email 3pids.

- **Matrix Authentication Service (MAS) support** is implemented: the provisioning API endpoints, synced provisioned email bindings, and SSO redirect-action forwarding (MSC3824, so a `register` action becomes `prompt=create` on the upstream OIDC request). Opened by @CEbbinghaus in (#266).

- A remote-server version API, a new client endpoint exposing the versions reported by remote servers, shipped by @dasha-uwu (d038c5bc8).

- `SSLKEYLOGFILE` support for the outbound client, for debugging federation TLS, also from @dasha-uwu (5f00e39cd).

- User reports can post to a configurable room instead of the admin room, so operators can moderate without holding server-admin, graciously contributed by @x86pup. Opened by @iwalkalone69 in (#180).

- The nginx reverse-proxy example collapses its duplicate 443 and 8448 server blocks into one, tip of the hat to @Daniel15 in (#487).

- Push gateway resolution now runs through the validating resolver, so the private-IP CIDR denylist (`ip_range_denylist`) applies to pusher delivery, including redirect hops (28e08c261). Operator note: this silently stops delivery to any push gateway on a private or loopback address (a localhost Sygnal, a LAN ntfy or UnifiedPush) until `ip_range_denylist` is adjusted to permit it.

- **Spec compliance** advanced across many endpoints: stable `/v1/mutual_rooms` with count and paging (MSC2666, cd5004e15); rich-text room topics preferring the `m.topic` block with legacy fallback and indexed for search (MSC3765, fc29fe4e3); extended profile fields with enforced size and grammar limits (MSC4133, 03b9909ed); invite and knock stripped state carried as full federation PDUs so a receiver can bind the create event (MSC4311, d2c473fd4); a registration terms stage (MSC1692, opened by @erebion in #289); appservice device management (MSC4190, opened by @ngophuocloi-miracle-aavn in #488); the user-report endpoint returning 200 for unknown users to deter enumeration (MSC4277, 6ddd59016); per-requester thread bundles with `current_user_participated` and the full `latest_event` (MSC3816, 3c3e65a7c); search results populating surrounding event context and pagination tokens (945e79cb8); device-list updates flushed to federation on key change (eff8e521d); and federation `get_missing_events` serving stored canonical JSON so unmodeled fields survive (e2eca4443).

### Bug Fixes

- The local server is now always exempt from `allowed_remote_server_names` and `forbidden_remote_server_names`. A 1.7.x allow-list that omitted the local name classified a local user's own events as coming from a forbidden remote and dropped them (fixes #489). Reported by @BurningEnlightenment. Sincere apologies to anyone whose own users went quiet.

- `/timestamp_to_event` (MSC3030) returned `M_NOT_FOUND` for valid searches because the room-scoped scan stopped on the first foreign-room key; fixed by @lingbohome in (#477).

- make-user-admin now grants the correct power level when the target is already in the admin room, with appreciation to @x86pup. Reported by @mio-19 in (#84).

- LDAP is skipped during UIA reauth for non-LDAP accounts, so resetting device keys on a non-LDAP account no longer triggers a filterless LDAP search, credit to @x86pup. Reported by @kuhnchris in (#255).

- A lost-wakeup in `until_shutdown` that could stall a service worker is fixed (22676a30b).

- `/timestamp_to_event` falls back to federation for local misses and forward start-edge queries (MSC3030, 914b16c98).

- Several federation membership fixes: a re-invite over a stale local ban is honored, an out-of-band invite rescission is applied, `join_authorised_via_users_server` is ignored for an existing member, custom `/join` body keys are merged into the member event, and kicking a non-member returns 403 (a37bd2448, b5101ac5a, 836831c01, 8e135be61, fae159522).

- Lazy-loaded incremental sync no longer drops changed members other than the syncing user (MSC4222, 0e5800c59).

- State resolution for pre-v12 rooms now begins iterative auth checks from an empty initial state, matching the handling used for version 12 rooms (954b0c32c).

- Per-room push rules now carry across a room upgrade (c852ae2f7).

- A device's local notification settings are removed on every device-deletion path (MSC3890, e0283e0dd).

- The single-event endpoint returns 404 rather than 403 for an event hidden from the requester (404b516f6).

- A client invite to a server lacking the room version returns `M_UNSUPPORTED_ROOM_VERSION` (MSC1866, 58691230f).

- Version 12 room upgrades omit the deprecated `predecessor.event_id` (MSC4291, 662506527).

- The `/context` end token is positioned so a backward page still includes the base event (f3fe502c6).

- The `[global.smtp]` config section is fixed (6061c5ca0).

- Notable for operators: the RocksDB storage engine is bumped to 11.1.1 (ab3276591), and the first 1.8.0 boot on an existing database performs a one-time timestamp-index rebuild from `pduid_pdu`, which lengthens first startup on large instances (41f0de074, 873f670b0).
