# Versioning

Every release we publish is a Zed version plus a **revision** counter, carried in
the semver pre-release field:

| Zed tag   | We publish | Meaning                                        |
| --------- | ---------- | ---------------------------------------------- |
| `v1.16.0` | `1.16.0-0` | first publish of that upstream code            |
|           | `1.16.0-1` | our fix on top of the *same* upstream code     |
|           | `1.16.0-2` | another fix                                    |
| `v1.16.1` | `1.16.1-0` | first publish of the next upstream release     |

We never publish a bare `1.16.0`. That is the whole trick, and it is worth being
precise about why.

## The problem

Versions used to be Zed's tag verbatim: Zed `v1.15.0` published as `1.15.0`. So
when we shipped `1.15.0` with missing font assets, there was no version left to
fix it with:

- `1.15.1` is Zed's next patch number. Taking it means our `1.15.1` and Zed's
  `1.15.1` are different code, and the next sync collides with itself.
- `1.15.0-fix.1` sorts *below* `1.15.0` — a pre-release is older than the
  release of the same triple. Cargo would never pick it up, so every user would
  have to pin `=1.15.0-fix.1` by hand, and pinning kills future updates.
- Bumping to `1.16.0` burns a minor version we don't own; upstream's real
  `1.16.0` then has nowhere to go.

There is genuinely no room between `1.15.0` and Zed's next tag. The fix is to
never occupy the whole triple in the first place.

## Why `-0` works

`1.16.0-0` is below `1.16.0`, which leaves the entire `-1`, `-2`, … range above
it and still below whatever Zed tags next:

```
1.16.0-0  <  1.16.0-1  <  1.16.0-2  <  1.16.0  <  1.16.1-0  <  1.16.1
   ^ us        ^ us         ^ us      ^ never      ^ us        ^ never
                                        ours                    ours
```

Revisions are *numeric* semver identifiers, so they compare as numbers, not as
text: `1.16.0-10` is above `1.16.0-9`, not between `1.16.0-1` and `1.16.0-2`.
The counter can run as long as it needs to.

## What this means for you

```toml
[dependencies]
gpui-unofficial = "1.16.0-0"
```

A requirement has to name a pre-release to be *offered* pre-releases, so the
`-0` is not optional — `gpui-unofficial = "1.16"` will not resolve at all now
that we publish nothing but pre-releases.

Given `gpui-unofficial = "1.16.0-0"`, Cargo resolves:

| Published  | Picked up | Why                                             |
| ---------- | --------- | ----------------------------------------------- |
| `1.16.0-1` | yes       | same `x.y.z`, higher revision — our fixes land automatically |
| `1.16.0-9` | yes       | numeric ordering                                |
| `1.16.1-0` | **no**    | a pre-release only matches a requirement with the same `x.y.z` |

So fixes to the Zed release you are on arrive with a normal `cargo update`, but
moving to a *new* Zed release is a deliberate edit of your `Cargo.toml`. That is
the real cost of this scheme, and it is the one thing to weigh against the
alternatives below. In exchange, a broken release stops being permanent.

Existing bare versions (`1.15.0` and earlier) stay exactly as published. The
scheme starts with the next Zed release; nothing needs yanking.

## Shipping a fix

Revisions are allocated from crates.io, not from a file in this repo — the
counter cannot drift:

```console
$ cargo xtask resolve-version --zed-tag v1.16.0          # highest published, or -0
1.16.0-0
$ cargo xtask resolve-version --zed-tag v1.16.0 --bump   # next revision
1.16.0-1
```

Without `--bump` the highest published revision is re-used, so re-running an
interrupted release resumes in place rather than skipping a number — the publish
step skips crates already on crates.io. `--bump` is therefore an explicit act:
run the **Sync Zed Releases** workflow from the Actions tab with `zed_tag` set to
the release you are fixing and `hotfix` checked.

If a revision was yanked, `--bump` still allocates above it. Yanked versions
disappear from the index, but their numbers are spent.

Releases from before this scheme (`1.15.0` and earlier) cannot be fixed this
way: `1.15.0-0` sorts *below* the `1.15.0` already published, so cargo would
never offer it to anyone. `resolve-version` warns if you try. Those releases
wait for Zed's next tag — which is exactly the hole this scheme closes going
forward.

## Zed preview tags

Zed's preview tags already use the pre-release field (`v1.16.0-pre`), so the
revision extends it as a dot segment instead of starting a second pre-release:
`1.16.0-pre.0`, `1.16.0-pre.1`. These sort among themselves and stay below
`1.16.0-0`, and `resolve-version` counts them separately from the stable line.

## Alternatives considered

- **`1.15.0-fix.1`.** Sorts below `1.15.0`, so it requires yanking `1.15.0` and
  pinning `=1.15.0-fix.1` in every dependent. Pinning blocks future updates.
- **`1.15.1-pre.1`.** Mechanically fine, but it claims Zed's next patch number
  and calls a fix a preview. When the real `1.15.1` lands, the two are unrelated
  code sharing a number.
- **Multiplying patch numbers by 100** (Zed `1.15.1` → us `1.15.100`, fixes at
  `1.15.101`). Plenty of room and ordinary release versions, so plain `"1.15"`
  requirements keep working — at the cost of version numbers that no longer look
  like the Zed release they track, and a rule you have to know to read them.
- **Waiting for the next release train.** What we did for `1.15.0`. Always
  available, but it makes every mistake permanent until Zed ships again.
