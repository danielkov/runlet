# Runlet design review log

The review loop used the user-requested command shape on every pass:

```sh
claude --permission-mode bypassPermissions --model claude-fable-5 -p "<adversarial review prompt>"
```

The reviewer read `DESIGN.md` from the repository on each pass and did not edit files. Findings were independently checked against the cited sections before changes were made.

## Pass 1 — NOT READY

Accepted blockers:

1. Statement-form control was unreachable under root-reachable lazy effects, and nested return was ambiguous.
2. The EBNF had an undefined nonterminal, duplicate expression rules, ambiguous returns, and signed-literal conflict.
3. Deterministic-result journaling contradicted replay wording.
4. Retry reuse used an undefined safety term and did not distinguish operation from dispatch identity.
5. Primary-error selection claimed fresh-run determinism it could not guarantee.
6. Operator types/failures were unspecified despite examples depending on list `+`.
7. Canonical float formatting was not pinned despite feeding identity.

Accepted additional findings covered registry-root shadowing, implicit Null-to-String, missing conversion nodes, union projection/narrowing, Map iteration, and an object-unsafe observer stream signature.

Resolution: v1 control became expression-only; the grammar was consolidated; replay/recomputation policy, operation/dispatch IDs, honest error-race semantics, operator tables, canonical number formatting, and the additional rules were added.

## Pass 2 — NOT READY

The reviewer marked 12 of 13 earlier issues resolved. Accepted remaining findings:

- retry reuse had to match the complete operation identity including dynamic key/generation;
- the suggested nullable-value fix required null-flow narrowing;
- equality and mixed chains also needed explicit rejection;
- reserved words and keyword-shaped property names needed normative lexical rules;
- cancellation transitions and newline continuation needed exact state/rule definitions.

All were corrected and paired with conformance fixtures.

## Pass 3 — NOT READY

All prior findings were verified resolved. Two new Phase 0 determinism blockers were accepted:

- JSON string escaping and raw Bytes canonical identity were incomplete; and
- list/string/bytes/object/map index/projection semantics and stable errors were missing.

Resolution: `DESIGN.md` now pins RFC 8785 presentation formatting, RCVE v1 for every portable value, and a normative projection table with `RL5203`, `RL5205`, and `RL5206`.

The pass's non-blocking notes were also addressed: keyed-hash triggering, orphaned completion events, unused effectful bindings, execution-class generation language, and run-header metadata.

## Pass 4 — READY

The reviewer verified both Phase 0 blockers and all prior notes resolved. It identified three fixture-level choices, which were made normative anyway:

- composite-to-String behavior for Map, Null, and Bytes;
- canonical argument-list envelopes for positional calls; and
- an exhaustive newline continuation set.

## Pass 5 — READY

The final confirmation found no new readiness blocker and verified that the three last normative choices were consistent with schemas, RCVE, operation identity, tool dispatch, implicit conversions, and grammar. Its only nit—same-line `} catch err {`—was made explicit in the lexical rules.

Final adversarial verdict: **READY** for the Phase 0 semantic executable model.
