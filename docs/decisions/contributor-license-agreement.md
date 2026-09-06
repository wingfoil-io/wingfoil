# Requiring a Contributor Licence Agreement

Status: **implemented**. The agreements are [`CLA.md`](../../CLA.md)
(individual) and [`CLA-CORPORATE.md`](../../CLA-CORPORATE.md) (corporate); the
gate is [`.github/workflows/cla.yml`](../../.github/workflows/cla.yml).

## The question

Wingfoil is Apache-2.0 and had no contributor agreement of any kind — nothing
in `CONTRIBUTING.md` about licensing, and exactly one `Signed-off-by` line in
the whole history. Six people have committed; five of them are not the project
owner, and one of those five has landed fifteen commits of *core* engine code
(`debounce`, `start_with`, `enumerate`, `take_while`, `step_by`, the stats and
zmq hot-path work, a Python binding).

Does that matter, and if so what fixes it?

## The decision

**Yes, and a CLA — not a DCO — fixes it.** Every contributor signs the
Individual CLA once, recorded automatically on their first pull request.
Companies whose employees contribute as part of their job sign the Corporate
CLA out of band.

## Why it matters

Absent an agreement, a contributor keeps the copyright in what they wrote and,
under Apache-2.0 §5, has implicitly licensed it to the project **under
Apache-2.0 and nothing else**. Three consequences follow, in ascending order of
how expensive they are to discover late:

1. **Dual licensing becomes impossible.** Offering the same code under a paid
   commercial licence requires holding rights to grant those terms. Under the
   implicit §5 grant alone, the project does not hold them for anyone else's
   contributions.
2. **A licence change cannot happen.** Moving future versions to a
   source-available licence — BSL, Elastic-style, anything — needs every
   copyright holder to agree that their code may be carried forward under the
   new terms. One unreachable contributor is enough to block it, or to force
   their code to be excised and rewritten.
3. **Chain of title is a diligence finding.** Any acquirer, and any
   counterparty asking for IP warranties in a commercial agreement, asks who
   owns the code. "Some was written by pseudonymous accounts who never
   asserted they had the right to contribute" is a poor answer, and there is
   no way to improve it retroactively without finding those people.

What is *not* blocked, and is worth stating so the CLA is not asked to do more
than it does: Apache-2.0 is permissive, so proprietary modules can be built on
top of the existing tree regardless. The CLA is what keeps the *existing* tree
relicensable — an option, not a plan.

## Why a CLA and not a DCO

A DCO (the `Signed-off-by` line) is cheaper and less off-putting, and it does
address point 3 above: the signer certifies they have the right to submit the
code. It does **nothing** for points 1 and 2, because it grants no rights
beyond the inbound licence. Since the value at stake here is precisely the
ability to change the outbound licence later, a DCO would be the appearance of
the fix rather than the fix.

The cost is real and acknowledged: a CLA is friction on a first-time
contributor, and some people decline on principle. Accepted, on the grounds
that this project's realistic contributor pool is small and its licence
optionality is worth more than the marginal drive-by PR.

## Why these documents

Both are near-verbatim adaptations of the Apache Software Foundation's ICLA
and CCLA v2.0, which have been in use since 2004 and are the most widely
recognised text in this space — a contributor who has signed one before
recognises it and does not have to read it adversarially. Departures from the
Apache text, all deliberate:

- **The sublicense right is explained, not just granted.** Apache's §1 grants
  it in a list; ours says in plain words what it is for (distributing under
  other terms in future, commercial ones included) and what it cannot do
  (retract anything already published under Apache-2.0, which is irrevocable).
  A grant whose purpose is hidden in a word is the thing contributors are
  right to be suspicious of.
- **§8, "Your employer", is expanded well past Apache's one clause.** This
  project's contributors are disproportionately people employed in finance,
  where IP assignment clauses are aggressive and routinely reach work done
  outside hours on anything related to the employer's business. The clause
  spells out the three ways to be clear of it rather than assuming the reader
  knows.
- **English law**, matching where the project owner is.
- **The grantee is defined to include successors.** "Jake Mitchell … together
  with any successor or assignee to whom ownership of the project's copyrights
  is subsequently transferred" means incorporating later does not invalidate
  every signature gathered before it. Without that clause, forming a company
  would mean re-collecting the lot.

## The retroactive half

The workflow only gates *new* pull requests. The five existing contributors
hold rights that no bot can collect, so:

1. Ask each to sign, by opening an issue that @-mentions them with a link to
   `CLA.md`. Most sign without hesitation.
2. Where someone does not respond or declines, their contributions have to be
   reverted or independently rewritten before any licence change. The
   fifteen-commit contributor is the one that matters; the remaining four have
   six commits between them.
3. Until that is closed out, the project's licence optionality is *encumbered*.
   Record it honestly in any commercial conversation rather than discovering it
   in someone else's diligence.

This is strictly cheaper the earlier it is done, and it gets more expensive
every month the contributor list grows.

## Operational note: the ledger branch

Signatures live in `.github/cla/signatures.json` on an **orphan branch**,
`cla-signatures`, so a signature — which is a bot commit — never lands on
`main`, never triggers the Rust matrix, and never interleaves with release
history. That branch has to exist before the first pull request is gated:

```bash
git checkout --orphan cla-signatures
git rm -rf . && mkdir -p .github/cla
echo '{"signedContributors":[]}' > .github/cla/signatures.json
git add .github/cla/signatures.json
git commit -m "chore(cla): seed the signature ledger"
git push -u origin cla-signatures
git checkout main
```

Then, in the repository's branch-protection settings, add **`cla / Signature on
file`** to the required status checks for `main`. The workflow reports the
check either way; branch protection is what makes it blocking.

The action itself (`contributor-assistant/github-action`) was archived upstream
in March 2026 with no successor. It is pinned by SHA and works; the header
comment in the workflow records the alternatives, and the one behavioural gap
worth knowing — it records the PR *opener*, not each commit author.
