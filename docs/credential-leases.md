# Renewable credential leases

Sylvander treats a configured secret reference as a locator, never as a
long-lived credential value. Runtime resolves Provider credentials through a
short lease with four independently checked facts:

- immutable credential-registry generation;
- monotonically increasing external lease generation;
- issuance and expiry timestamps;
- zeroing secret bytes whose `Debug` representation is redacted.

The built-in environment/file adapter issues 30-second leases. Every Provider
request rechecks the active registry generation and renews at the configured
renewal boundary. File content can therefore rotate without rebuilding the
Agent. A registry head change immediately invalidates the cached lease even
when it has remaining time.

Renewal is fail-closed. A failed renewal never falls back to the previous
value, even if that value has not quite expired. An already-issued request
lease also rejects access once its clock reaches `expires_at`. A
deployment-supplied external provider (for example, a Vault adapter) implements
the same acquire/renew boundary, and Runtime validates its generations and
maximum five-minute TTL rather than trusting provider metadata. The repository
ships this injection contract and the environment/file adapter; it does not
claim a built-in vendor-specific Vault client.

Production composition injects that boundary with
`ProviderCredentialSources::new(resolver, lease_provider)` and
`Runtime::boot_config_with_provider_credentials(config, sources)`. The
`RenewableExternalSecretProvider` instance is retained by the revision
provider, so both Agents present at boot and Agent revisions composed later
use the same live acquire/renew path.

Provider logs may contain Provider ID, credential generation, lease
generation, and expiry. They must never contain a binding locator, secret
reference, renewal token, or secret bytes.

## Channel credential audit

The channel implementations expose these credential boundaries:

| Channel | Credential slots | Operation boundary |
| --- | --- | --- |
| HTTP | bearer token | each `/chat` authentication |
| WebSocket | bearer token | HTTP upgrade |
| DingTalk | app key + app secret | Stream connect and access-token refresh |
| Telegram | bot token + webhook secret | outbound Bot API call and inbound webhook authentication |
| WeChat | callback token + encoding AES key + API secret | inbound signature/decrypt/encrypt and outbound API token refresh |

`channel.id` is the stable instance scope. Two bots of the same transport must
never share a mutable lease cache unless they explicitly reference the same
credential binding. Rotation is generation-based and requires no process or
channel restart.

The common channel contract has the same rules as the Provider path:

1. The composition root registers named credential slots for one channel
   instance; channel objects receive only an object-safe lease source.
2. A lease source returns an atomic bundle when a protocol requires multiple
   values to match.
3. Authentication and outbound calls read the lease at the operation boundary,
   not when the server boots.
4. Expiry or renewal failure rejects that operation. There is no silent use of
   an old token and no cross-instance fallback.
5. The built-in source emits content-safe operational diagnostics for a
   successful lease: instance ID, slot count, credential generation, lease
   generation, and expiry. Adapter failures use generic lease/authentication
   errors. This path does not currently claim a separate durable channel-lease
   audit ledger.
6. Tests rotate every slot while the channel remains running, verify the old
   value is rejected, verify the new value is accepted, and exercise
   renewal-failure and post-expiry paths.

Until a transport consumes this contract at its operation boundary, it must
not be described as supporting live credential rotation.
