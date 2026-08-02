# AgentRelay message catalogue

The strings a person sees at 8am, in one place, versioned, and checked. This
directory is **data plus a validator** — no code from `daemon/src` reaches in
here, and nothing here reaches into the daemon. Another process wires it into
Rust; this is designed so that wiring is trivial (see *Loading from Rust*).

`en.json` is the source of truth. Every other file is measured against it by
`check.mjs`. A translation is not "done" because someone wrote words in it — it
is done when the validator passes, because the ways a catalogue breaks are not
the ways prose breaks.

## Running the validator

Plain Node, no dependencies, no network.

```
node check.mjs                      # validate the whole catalogue; exit 0 if clean
node check.mjs fixtures/x.he.json   # validate one file against en.json
```

Exit code is `0` only when every file is clean, non-zero otherwise. It is meant
to run in CI on every change to any `*.json` here. It prints, per file, either a
green line or a numbered list of exactly what is wrong and where.

## What was extracted, and from where

Every string here is a real string the program prints. Nothing was invented. The
`_comment` on each key in `en.json` names where it appears. Sources, all under
`daemon/src/`:

| file | what it contributes |
|---|---|
| `main.rs` | the whole CLI surface — `status`, `run`, `doctor`, `validate`, `init`, `hook`, `install-hook`, `fits`, `begin`, `done`, `report`, `fix`, `checkpoints`, `serve`, and the top-level error prefix |
| `report.rs` | the morning report — provenance tags, headlines, every body line, the footer |
| `state.rs` | mission-state meanings and illegal-transition reasons |
| `cost.rs` | budget descriptions and the kill-switch stop reasons |
| `isolation.rs` | the isolation on/off/network descriptions and the Docker blocker |
| `notify.rs` | the notification privacy descriptions and the public-server warning |
| `drift.rs` | drift summaries, signals, and the flagged-area explanations |
| `fixloop.rs` | the fix-prompt text sent to the agent and the give-up reasons |
| `which.rs` | install hints for `claude` / `git` / `node` |
| `hook.rs` | the PreToolUse denial texts |
| `daemon.rs` | the config-invalid refusal, the budget refusal, the no-baseline refusal |

162 keys in total. A handful of pure git commands (`git reset --keep …`) are
generated in `checkpoint.rs` and interpolated into `done.undo` / `done.rewind`
as `{cmd}` — the label is translated, the command is not.

## Interpolation

One syntax, everywhere: `{name}` in single braces. There are no literal braces
anywhere in the catalogue.

**A translation must use exactly the same placeholders as `en.json` for the same
key** — same names, none added, none dropped. This is the check that prevents the
classic crash: a Rust format string references `{sha}`, the German line forgot
it, and the day that line first prints in German the program panics. `check.mjs`
compares the placeholder sets and fails on any difference. Which key takes which
variables is documented in each `en.json` `_comment`.

## Plurals

English has two plural forms; most languages do not. Shipping `1 unit(s)` is the
lazy answer and it reads as broken in every language including English.

A message value that is an **object keyed by CLDR plural categories** is a plural
message; the count that selects the form is the `{count}` variable. Example:

```json
"report.headline.ok": {
  "one":   "AgentRelay: {count} unit done, all validators passed",
  "other": "AgentRelay: {count} units done, all validators passed"
}
```

Each language must supply **exactly** the categories its language requires — no
more, no fewer — or some real number selects a form nobody wrote. The validator
enforces this table (from the Unicode CLDR plural rules):

| lang | required categories |
|---|---|
| en, es, fr, de | `one`, `other` |
| ru | `one`, `few`, `many`, `other` |
| he | `one`, `two`, `many`, `other` |
| ar | `zero`, `one`, `two`, `few`, `many`, `other` |

So the Russian `1 файл / 2 файла / 5 файлов` distinction, the Arabic dual, and
the Hebrew two/many split are all real forms, not an English `(s)` in disguise.
(French CLDR also defines a `many` category for large compact numbers; AgentRelay
never formats those, so French is treated as `one`/`other`. If that ever changes,
add `many` to the French row and the validator will demand it.)

## Bidi — the part that looks like line-noise and isn't

This is the section to read before touching `he.json` or `ar.json`.

Hebrew and Arabic run right-to-left. But the things AgentRelay talks about —
file paths, git SHAs, shell commands, URLs, `$5.00`, ntfy topics, timestamps —
are **left-to-right**, always, even inside an RTL sentence. Drop one into RTL
text with no protection and the bidi algorithm reorders it on screen. A concrete
failure: the Hebrew report line for a checkpoint wants to say

> run `git reset --keep abc1234` to undo it

Written naively in an RTL paragraph, the terminal renders the command with its
pieces flipped and the `abc1234` on the wrong side — it looks like a font bug,
and worse, a user who copies it can copy something subtly wrong.

The fix is a **bidi isolate**: wrap the LTR run in `U+2066` (LRI, left-to-right
isolate) … `U+2069` (PDI, pop). "Isolate" is the key word — unlike the older
embedding marks, an isolate also stops the LTR run from disturbing the direction
of the RTL text around it. The rules this catalogue follows:

- **Known-LTR content → `U+2066 … U+2069` (LRI/PDI).** Paths, SHAs, commands,
  `$` amounts, program names, config keys, URLs, and the literal product name
  `AgentRelay` when it sits inside RTL text. In the JSON these appear as the
  characters `⁦…⁩`.
- **Placeholders whose direction varies → `U+2068 … U+2069` (FSI/PDI).** When a
  placeholder holds *another translated string* (`{reason}`, `{why}`,
  `{meaning}`, `{summary}`), it may be RTL or LTR at runtime. First-Strong
  Isolate picks the direction from the content's first strong character, so it is
  right either way. In the JSON these are `⁨…⁩`.
- **`U+200F` (RLM)** is for nudging the *base* direction of a line at a boundary;
  it does **not** wrap an embedded run and is not a substitute for an isolate.
  This catalogue standardises on isolates for embedded content.

So the checkpoint line in `he.json` is:

```
נקודת-שמירה ⁦{sha}⁩ (⁦{id}⁩) — הריצו ⁦git show {sha}⁩ כדי לקרוא אותה
```

— the SHA, the id, and the whole `git show …` command each sit in their own
`⁦…⁩` island, and render correctly left-to-right inside the Hebrew sentence. The
same discipline gives `git reset --keep abc1234` (arriving via `{cmd}` in
`done.undo`) its own isolate, so it renders intact.

The validator turns this discipline into a hard check for RTL files:

- **Balanced isolates.** Every opener (`U+2066/2067/2068`) has a matching
  `U+2069`, and none closes early. An unbalanced isolate leaks its direction into
  the rest of the line — one of the ugliest and hardest-to-spot bugs.
- **No naked LTR in RTL text.** In any string that contains RTL letters, a Latin
  run or a `{placeholder}` found *outside* an isolate is an error. Pure-LTR
  values (a bare `AgentRelay`, a `{sha} {id}` line with no RTL around it) are
  exempt, because there is no RTL flow for them to break.

## Severity words

`VERIFIED` / `RECORDED` / `ESTIMATED` are not decoration; they are the product's
central claim. `VERIFIED` means a command ran and its exit code decided the line
— evidence. `RECORDED` means the supervisor observed it directly. `ESTIMATED`
means a number from a rule of thumb, never a measurement. If a translation
collapses the three into two, the report stops distinguishing *proof* from
*guess*, which is the one thing it exists to do.

The three are kept genuinely distinct in every language:

| en | he | ar | es | fr | de | ru |
|---|---|---|---|---|---|---|
| VERIFIED | מאומת | مُتحقَّق | VERIFICADO | VÉRIFIÉ | VERIFIZIERT | ПРОВЕРЕНО |
| RECORDED | מתועד | مُسجَّل | REGISTRADO | ENREGISTRÉ | AUFGEZEICHNET | ЗАФИКСИРОВАНО |
| ESTIMATED | מוערך | مُقدَّر | ESTIMADO | ESTIMÉ | GESCHÄTZT | ОЦЕНКА |

Note where it strains: in Hebrew, *verified* (מאומת) and *confirmed/validated*
(מאושר) are near-synonyms, so `RECORDED` deliberately uses מתועד ("documented/
recorded") to hold the three apart. In Arabic the same care was taken with
مُتحقَّق / مُسجَّل / مُقدَّر. The `report.footer` in each language re-uses these exact
words when it explains the tags, so the key and its explanation can never drift.

## Translation quality — read this before trusting a language

Only Hebrew was written by someone fluent in the language (the author's, and the
explicit priority of the project). Be honest about the rest:

| lang | status |
|---|---|
| **en** | source of truth |
| **he** | **written by a Hebrew speaker.** Grammar, RTL isolation and plural forms reviewed by hand. |
| **es**, **fr**, **de** | machine translation, **not reviewed by a native speaker.** Structurally valid; wording is plausible but unverified. |
| **ru** | machine translation, **not reviewed by a native speaker.** The four plural declensions in particular need a native check. |
| **ar** | machine translation, **not reviewed by a native speaker.** RTL isolation is verified mechanically, but the six plural forms use approximated Modern Standard Arabic inflection and genuinely need a native reviewer. |

"Structurally valid" means only what the validator can prove: keys match,
placeholders match, plural categories are complete, isolates are balanced and
LTR content is protected. It does **not** mean the words are right. Do not mark
any of es/fr/de/ru/ar as reviewed until a native speaker has actually read them.

## Glossary

Following the plan (§י), a few deep-jargon terms are kept as isolated Latin
inside RTL text rather than translated, because a half-translated git vocabulary
is harder to act on than the real one: `commit`, `stash`, `worktree`, `hook`,
`Docker`, `PATH`, `proxy`, `IPC`, and literal config keys / commands. `mission`
and `checkpoint` **are** translated where a natural word exists (Hebrew
משימה / נקודת-שמירה), because they read as ordinary nouns, not tool names.

## Loading from Rust

The runtime shape is deliberately dull so `serde` needs no custom code. A
translation file is a map from key to message, where a message is either a string
or a map of plural forms:

```rust
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Message {
    Plain(String),
    Plural(std::collections::BTreeMap<String, String>), // "one","two",… → text
}

// he.json, ar.json, … :
type Catalogue = std::collections::HashMap<String, Message>;
```

`en.json` is the one file with a richer shape — each entry is
`{ "_comment": …, "value": <Message> }` — because it carries the translator
notes. Runtime never needs the comments: either load `en.json` and read `.value`,
or generate a plain `en` catalogue from it. Keys beginning with `_` (there is one,
`_meta`) are metadata and are skipped by the validator; a loader should skip them
too.

Selecting a plural form at runtime is the caller's job: map the count to its CLDR
category for the active language and look it up, falling back to `other`.

## Fixtures

`fixtures/` holds six files that are each a real catalogue file with exactly one
deliberate break, to prove each check actually fires. Every one must make
`check.mjs` exit non-zero:

```
node check.mjs fixtures/missing-key.es.json        # a key present in en.json is gone
node check.mjs fixtures/stale-key.es.json          # a key en.json no longer has
node check.mjs fixtures/var-mismatch.es.json       # {sha} renamed to {hash}
node check.mjs fixtures/plural-missing.ru.json     # Russian "many" form dropped
node check.mjs fixtures/unbalanced-isolate.he.json # a closing U+2069 removed
node check.mjs fixtures/missing-isolation.he.json  # an LTR command left naked in Hebrew
```

A check that is never seen to fail is a check you cannot trust; these are how you
see them fail.
