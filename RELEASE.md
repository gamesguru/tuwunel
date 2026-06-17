# Tuwunel 1.7.1

June 5, 2026

### New Features & Enhancements

- **A new federation data-fetching service** improves reliability in rooms whose history is spread across many servers by locating missing events through concurrent queries. It ranks candidate servers by room-membership popularity and recent reachability, and reuses requests already in flight instead of issuing duplicates. Resolved state for outlier events, fetch and backoff decisions, and per-server reachability ("Peer Status") are now cached persistently, and auth-chain, state, prev-event, and backfill fetches all run through it. Servers that used to re-request the same uncacheable lookups should see far less repeated federation traffic.

- **OIDC device authorization grant (RFC 8628)** lets users sign in on input-constrained devices. The MSC4191 account-management action set is now complete with a deep-linked cross-signing reset, and MSC3861 OAuth 2.0/OIDC is advertised on `/versions`. The token endpoints and refresh-token lifecycle were reworked, dynamic client registration is opt-in and validates submitted client metadata, and device-scope binding requires PKCE. The OIDC authorization-server chapter of the documentation was expanded to match.

- **Several additional MSCs** land this cycle: MSC3980 (`event_fields` trimming on `/sync`), MSC3860 (media download redirects), MSC4311 (`origin_server_ts` on the stripped create event), MSC1219 (key backup storage conformance), MSC2659 (appservice ping error codes), MSC3550 (`403 M_FORBIDDEN` allowed on profile lookup), and a stable `m.forget_forced_upon_leave` capability (MSC4267). MSC4380 invite blocking now also covers invites delivered through `/sync` and `createRoom`.

- **Support-contact discovery gains a PGP field and policy links** (MSC4439, MSC4266), graciously contributed by @x86pup. The `/.well-known/matrix/support` endpoint can now advertise a `pgp_key` per contact (with raw key material rejected) and link support policies, and multiple support contacts can be configured with validation.

- @dasha-uwu added an `admin media preview` command for debugging URL previews, retired blurhashing, dropped the legacy media-preview redirect, and removed the deprecated server-keys endpoint.

- Sliding sync (v5) now retracts departed and left rooms from the list and adds re-invited rooms back, so clients track membership churn without a full resync.

- A device may now hold multiple access tokens, for easier rotation and concurrent sessions.

- `/context` can optionally resolve events it has not yet received over federation, and outbound HTTP compression gained per-direction opt-out switches.

- An admin command to purge every room containing a given user was added, raised by @winyadepla in (#472).

- Documentation for `ip_source_trusted_subnets` now warns about accidentally including a proxy in the trusted set, courtesy of @BVollmerhaus in (#468).

- Diagnostic admin command suites were added for the federation fetcher and Peer Status, and the runtime can dump tokio and getrusage metrics to JSON at exit.

### Bug Fixes

- A regression introduced with `ip_source` in 1.6.1 blocked locally-connected appservices and other loopback clients (#465). Loopback peers and trusted-peer subnets now bypass the configured `ip_source`, including over the Unix-socket listener, and the `axum-client-ip` dependency was replaced with inlined helpers. Sincere apologies to everyone whose bridges went quiet.

- Remote room directory and summary lookups are more resilient over federation: the room-summary fallback now tries every `via` server (5c9998374), and a failed remote `publicRooms` request now returns a `502` (9a879776c).

- Thank you @x86pup for reporting in (#466) that a bad `unix_socket_path` produced an opaque startup failure; listener initialization errors now name the offending path.

- `!admin query oauth associate` replied with an empty message and did nothing, reported by @Vazgen005 in (#467). It now emits a confirmation and accepts a `force` flag.

- @dasha-uwu fixed a compression configuration option that could accidentally disable client-side decompression.

- Several federation correctness fixes: the federation lock is now held across the invite residency check to close a join/unban race (add512b76); a `send_join` response that omits state fails over to other servers (9c158d3a0); each transaction's PDUs are sorted topologically before handling (91218e1df); and references outside the auth graph are treated as non-edges during resolution (664391995).

- Knock membership is now persisted and a remote re-knock re-drives to reconcile state; per-PDU backfill errors are isolated so one bad event no longer aborts the batch; and thread redaction walks through the redacted target.

- Media fetches and URL previews now honor CIDR denylists for the addresses they resolve to (af1266af3, 554557cf3). Buffered outbound responses are size-bounded, and federation key lookups are bounded and backed off.

- Configuration handling improved: an unreadable `client_secret_file` now reports the path and IO error (844f123c7), matched keys can be excluded from the "unknown to tuwunel" warning (6bbfd0a93), and packaged builds no longer drop their `malloc_conf` tuning (de0eb1d2e).
