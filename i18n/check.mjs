#!/usr/bin/env node
// AgentRelay i18n validator. Plain Node, no dependencies, no network.
//
// Why this file is longer than a string table deserves: a translation catalogue
// fails in ways a human review does not catch. A {sha} placeholder dropped from
// one language is a runtime crash the day that line is printed in that language,
// not a typo. A Hebrew sentence with an un-isolated `git reset --keep abc1234`
// inside it renders as visual garbage that looks like a font bug. This validator
// turns each of those into a loud, early, deterministic failure.
//
// Run:
//   node check.mjs                      -> validate the real catalogue (exit 0 if clean)
//   node check.mjs fixtures/x.he.json   -> validate one file against en.json (used to
//                                          prove each check actually fires)
//
// Exit code is non-zero if anything failed. Nothing is written; nothing is sent.

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, basename } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

// ── language facts ────────────────────────────────────────────────────────────
// Which languages are right-to-left. Only these get the bidi-isolation checks,
// because embedding LTR content is only a problem when the base direction is RTL.
const RTL = new Set(["he", "ar", "fa", "ur"]);

// The CLDR plural categories each language is REQUIRED to provide. English has
// two; Hebrew has four; Arabic all six; Russian four. A plural message in a given
// language must supply exactly this set — no more, no fewer — or some real count
// will select a form that was never written. (Values from the Unicode CLDR plural
// rules; see README "Plurals".)
const PLURAL_CATEGORIES = {
  en: ["one", "other"],
  es: ["one", "other"],
  fr: ["one", "other"],
  de: ["one", "other"],
  ru: ["one", "few", "many", "other"],
  he: ["one", "two", "many", "other"],
  ar: ["zero", "one", "two", "few", "many", "other"],
};

// Bidi isolate marks (all inside the U+2066..U+2069 block the plan calls for).
const LRI = "⁦"; // Left-to-Right Isolate  — wrap KNOWN-LTR content (paths, SHAs, commands, $amounts)
const RLI = "⁧"; // Right-to-Left Isolate
const FSI = "⁨"; // First-Strong Isolate   — wrap content of UNKNOWN direction (a nested translated string)
const PDI = "⁩"; // Pop Directional Isolate — closes any of the three
const OPENERS = new Set([LRI, RLI, FSI]);

// ── helpers ───────────────────────────────────────────────────────────────────
function loadJson(path) {
  const raw = readFileSync(path, "utf8").replace(/^﻿/, "");
  return JSON.parse(raw);
}

// A message value is either a plain string or a plural object {one, other, ...}.
function isPluralValue(v) {
  return v && typeof v === "object" && !Array.isArray(v);
}

// Every {placeholder} name used across all forms of a message.
function varsOf(value) {
  const forms = isPluralValue(value) ? Object.values(value) : [value];
  const set = new Set();
  for (const f of forms) {
    if (typeof f !== "string") continue;
    for (const m of f.matchAll(/\{([a-zA-Z0-9_]+)\}/g)) set.add(m[1]);
  }
  return set;
}

// en.json entries are {_comment, value}; translations are the bare value. This
// pulls the message out of whichever shape a file uses.
function messageOf(entry) {
  return isPluralValue(entry) && "value" in entry && !("one" in entry || "other" in entry)
    ? entry.value
    : entry;
}

function langOf(file) {
  const segs = basename(file).replace(/\.json$/, "").split(".");
  for (const s of segs) if (PLURAL_CATEGORIES[s]) return s;
  return segs[segs.length - 1];
}

const hasRtlStrong = (s) => /[֐-׿؀-ۿ܀-ݏ]/.test(s);
const hasLatinLetter = (s) => /[A-Za-z]/.test(s);

// ── the checks ──────────────────────────────────────────────────────────────
// Each returns an array of human-readable problem strings (empty = clean).

// Isolate marks must be balanced and never close what was never opened. An
// unbalanced isolate leaks its direction into the rest of the line. Applies to
// every language (English simply has none, so it passes trivially).
function checkBalancedIsolates(key, form, where, out) {
  let depth = 0;
  for (const ch of form) {
    if (OPENERS.has(ch)) depth++;
    else if (ch === PDI) {
      depth--;
      if (depth < 0) {
        out.push(`${where} ${key}: a bidi PDI (U+2069) closes an isolate that was never opened`);
        return;
      }
    }
  }
  if (depth !== 0) out.push(`${where} ${key}: ${depth} bidi isolate(s) opened but never closed (missing U+2069)`);
}

// In an RTL string that also contains RTL text, any LTR run — a literal Latin
// word/command OR an interpolated {placeholder} that will carry a path/SHA — must
// sit inside an isolate. We detect a violation as a Latin letter or a '{' found at
// isolate depth zero. Pure-LTR values (a bare product name, a SHA-only line) have
// no RTL to break, so they are exempt.
function checkRtlIsolation(key, form, out) {
  if (!hasRtlStrong(form)) return; // nothing to protect
  let depth = 0;
  let i = 0;
  while (i < form.length) {
    const ch = form[i];
    if (OPENERS.has(ch)) { depth++; i++; continue; }
    if (ch === PDI) { depth = Math.max(0, depth - 1); i++; continue; }
    if (depth === 0) {
      if (ch === "{") {
        const m = form.slice(i).match(/^\{([a-zA-Z0-9_]+)\}/);
        const name = m ? m[1] : "?";
        out.push(`${key}: placeholder {${name}} is not bidi-isolated — in RTL text it can carry an LTR value (path/SHA/command) that reorders the sentence. Wrap it: ⁦{${name}}⁩ (LTR content) or ⁨{${name}}⁩ (text of unknown direction).`);
        return;
      }
      if (/[A-Za-z]/.test(ch)) {
        const word = form.slice(i).match(/^[A-Za-z][\w.\/-]*/)[0];
        out.push(`${key}: the LTR run "${word}" sits directly in RTL text without a bidi isolate. Wrap it in ⁦…⁩ so it does not visually reorder.`);
        return;
      }
    }
    i++;
  }
}

function checkFile(enKeysAndVars, enEntries, file) {
  const lang = langOf(file);
  const problems = [];
  const data = loadJson(file);

  const enKeys = new Set(enKeysAndVars.keys());
  const trKeys = new Set(Object.keys(data).filter((k) => !k.startsWith("_")));

  // 1. missing keys
  for (const k of enKeys) if (!trKeys.has(k)) problems.push(`missing key: ${k}`);
  // 2. stale keys (no longer in en.json)
  for (const k of trKeys) if (!enKeys.has(k)) problems.push(`stale key not in en.json: ${k}`);

  for (const k of enKeys) {
    if (!trKeys.has(k)) continue;
    const enValue = enKeysAndVars.get(k).value;
    const trValue = messageOf(data[k]);

    // 3. plural shape + required categories
    const enPlural = isPluralValue(enValue);
    const trPlural = isPluralValue(trValue);
    if (enPlural && !trPlural) {
      problems.push(`${k}: en.json is pluralized but the translation is a single string — a real count will read wrong`);
      continue;
    }
    if (!enPlural && trPlural) {
      problems.push(`${k}: translation is pluralized but en.json is a plain string`);
      continue;
    }
    if (enPlural && trPlural) {
      const need = PLURAL_CATEGORIES[lang] || ["one", "other"];
      const have = Object.keys(trValue);
      for (const c of need) if (!have.includes(c)) problems.push(`${k}: missing plural form "${c}" (required for '${lang}')`);
      for (const c of have) if (!need.includes(c)) problems.push(`${k}: unexpected plural form "${c}" for '${lang}' (allowed: ${need.join(", ")})`);
    }

    // 4. interpolation variables must match exactly (the classic crash)
    const enVars = enKeysAndVars.get(k).vars;
    const trVars = varsOf(trValue);
    for (const v of enVars) if (!trVars.has(v)) problems.push(`${k}: translation dropped placeholder {${v}} that en.json uses`);
    for (const v of trVars) if (!enVars.has(v)) problems.push(`${k}: translation introduced placeholder {${v}} that en.json does not have`);

    // 5. bidi: balanced isolates everywhere; isolation of LTR runs in RTL langs
    const forms = trPlural ? Object.values(trValue) : [trValue];
    for (const form of forms) {
      if (typeof form !== "string") { problems.push(`${k}: a plural form is not a string`); continue; }
      checkBalancedIsolates(k, form, "bidi", problems);
      if (RTL.has(lang)) checkRtlIsolation(k, form, problems);
    }
  }

  return { lang, problems };
}

// ── run ───────────────────────────────────────────────────────────────────────
const enPath = join(HERE, "en.json");
const en = loadJson(enPath);

// Precompute en keys, their message value, and their variable set.
const enIndex = new Map();
for (const [k, entry] of Object.entries(en)) {
  if (k.startsWith("_")) continue;
  const value = messageOf(entry);
  enIndex.set(k, { value, vars: varsOf(value) });
  // en itself must have balanced isolates (it has none, but a future edit might).
  const forms = isPluralValue(value) ? Object.values(value) : [value];
  const p = [];
  for (const f of forms) checkBalancedIsolates(k, f, "bidi(en)", p);
  if (p.length) { console.error("en.json is itself malformed:"); for (const x of p) console.error("  - " + x); process.exit(2); }
}

const args = process.argv.slice(2);
let files;
if (args.length) {
  files = args.map((a) => (a.includes("/") || a.includes("\\") ? a : join(HERE, a)));
} else {
  files = readdirSync(HERE)
    .filter((f) => f.endsWith(".json") && f !== "en.json")
    .map((f) => join(HERE, f));
}

let failed = false;
for (const file of files) {
  const { lang, problems } = checkFile(enIndex, en, file);
  const name = basename(file);
  if (problems.length) {
    failed = true;
    console.error(`\n✗ ${name} (${lang}) — ${problems.length} problem(s):`);
    for (const p of problems) console.error("   - " + p);
  } else {
    console.log(`✓ ${name} (${lang}) — ${enIndex.size} keys, all checks passed`);
  }
}

if (failed) {
  console.error("\ni18n validation FAILED.");
  process.exit(1);
}
console.log("\ni18n validation passed.");
