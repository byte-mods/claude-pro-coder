# Example 4 — Pure docs change (fast-path)

## Prompt

> The README's "Install" section says `pip install foo` but the package
> name is actually `foo-bar`. Fix that.

## What to expect

The skill announces **`> fast-path: README typo, single-line edit`** at
the top of the response — explicit, so you know it's not running the
full loop.

What runs:

1. **P1 (lite):** confirm the fact — read the README, confirm the typo
   exists, confirm `foo-bar` is the actual package name (e.g. via
   `pyproject.toml` or `setup.py` or by reading the install instructions
   anywhere else they appear).
2. **Edit.** One-line change.
3. **P5 (lite):** if the typo's correct version is documented anywhere
   else, those copies get updated too. No code-map update because no
   documented fact changed (a typo correction is not a fact change).

What's skipped:

- **P3 plan presentation.** Trivial change, no planning value.
- **P4.5 super-qa.** Pure typo fix, no behaviour change.
- **P6 section boundary.** Single-action; no context to reset.

The skill outputs a one-paragraph confirmation in the clean format,
typically just a *What changed* table with one row.

## What the skill is doing internally

- **Why fast-path exists:** running super-qa for a typo fix burns a
  context spawn for zero adversarial yield. The fast-path rule scopes
  out trivial changes explicitly.
- **The "trivial" definition is strict:** typo, doc tweak, single-line
  rename inside one file, formatting-only changes. **Anything ambiguous
  is not trivial.** If a "typo fix" turns out to touch a string that's
  consumed as a public API, the skill stops mid-implementation, switches
  to the full loop, and notifies you.
- **Why no code-map update for pure typos:** code-map notes describe
  *what is*, anchored at `file:line`. A README typo correction does not
  change the underlying code or its invariants. The note does not need
  to be touched.
