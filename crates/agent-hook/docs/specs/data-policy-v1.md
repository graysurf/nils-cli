# Data-policy v1

`agent-hook data-policy evaluate --format json` is a stateless, deterministic
classifier for one DSH-normalized candidate. It accepts one strict JSON object
up to 1 MiB and returns one `agent-hook.data-policy.decision.v1` envelope.
Unknown fields, invalid identities or digests, duplicate class rules, and
unsupported schemas fail closed with exit 65.

The request binds the phase, public source and sink IDs, exact session and call
lineage, workspace digest and generation, turn and step, stable rule IDs, rule
actions, and candidate.
The response echoes a `request:<32-lowercase-hex>` digest of the exact input.
Consumers must match that value to their own request before accepting the
decision. Audit output contains only action, stable code, class/source/sink
IDs, and payload/binding digests; candidate content and execution IDs are never
copied into the response. `matched_rule_ids` identifies the exact rules that
contributed to the decision without retaining their governed input.

## Classifier corpus

The executable corpus in `tests/data_policy.rs` fixes these boundaries:

| Shape | Classified | Intentionally allowed |
| --- | --- | --- |
| Structured JSON | Exact/suffix credential keys and known token/private-key signatures | Counters such as `token_count` and unlabeled values with no governed signature |
| Text | Known signatures and Linux, macOS, or Windows machine paths, including paths embedded in a line | Short token-prefix documentation and generic `/home/` examples |
| Streamed text | Adjacent strings or `{\"text\": ...}` blocks are scanned both separately and as one ordered projection | Unordered fields are not concatenated |
| Binary-shaped JSON | Bytes beneath a governed credential key are sensitive | Opaque base64 is not decoded or guessed from entropy |
| Provider opaque reference | Sensitive classification still applies | Machine-path classification is suppressed only for the explicit `provider.opaque-reference` source |

The intentionally allowed cases are the documented false-negative boundary:
the classifier does not infer secrets from entropy, decode arbitrary binary,
or label every identifier-like string. Callers that know a field is sensitive
must use a governed key or a protected sink rule. The documented false-positive
boundary is equally explicit: recognizable credential signatures and governed
credential keys are classified even when a fixture says the value is
synthetic. Tests therefore use synthetic values and never real credentials.

`redact` replaces the complete governed projection; `quarantine` returns only
`quarantined: true` and the non-bearer payload digest. `deny` and `allow` return
no replacement. When multiple classes match, the deterministic precedence is
deny, quarantine, redact, then allow.
